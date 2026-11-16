//! Unit tests for the agent loop entry points and helpers.
//!
//! Extracted from `loop_/mod.rs` so production assembly and the large
//! behavioral suite can evolve independently.

use super::{
    apply_text_tool_prompt_policy, estimate_history_tokens, make_query_summary,
    maybe_inject_channel_delivery_defaults, truncate_tool_result,
};

use crate::agent::history::DEFAULT_MAX_HISTORY_MESSAGES;
use crate::agent::tool_execution::{ToolDispatchContext, execute_one_tool};
use std::sync::Arc;
use tempfile::tempdir;
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};
use zeroclaw_providers::{ChatMessage, ToolCall};

zeroclaw_api::mock_tool_attribution!(
    CountingTool,
    CredentialOutputTool,
    EmptySuccessTool,
    RecordingArgsTool,
    DelayTool,
    FailingTool,
    NamedMockTool,
    CompletesAndSignalsTool,
    CancelsTurnTool,
    VerboseTool,
);

// ── maybe_inject_channel_delivery_defaults tests ──────────────

#[test]
fn cron_delivery_defaults_include_dingtalk_channel() {
    let mut args = serde_json::json!({
        "job_type": "agent",
        "prompt": "remind me later",
        "schedule": { "kind": "every", "every_ms": 60000 }
    });

    maybe_inject_channel_delivery_defaults("cron_add", &mut args, "dingtalk", Some("chat-42"));

    assert_eq!(
        args["delivery"],
        serde_json::json!({
            "mode": "announce",
            "channel": "dingtalk",
            "to": "chat-42",
        })
    );
}

#[test]
fn cron_delivery_defaults_do_not_guess_webhook_shape() {
    let mut args = serde_json::json!({
        "job_type": "agent",
        "prompt": "remind me later",
        "schedule": { "kind": "every", "every_ms": 60000 }
    });

    maybe_inject_channel_delivery_defaults("cron_add", &mut args, "webhook", Some("thread-42"));

    assert!(
        args.get("delivery").is_none(),
        "webhook delivery needs sender/thread context and must not reuse reply_target as to"
    );
}

// ── truncate_tool_result tests ────────────────────────────────

#[test]
fn truncate_tool_result_short_passthrough() {
    let output = "short output";
    assert_eq!(truncate_tool_result(output, 100), output);
}

#[test]
fn truncate_tool_result_exact_boundary() {
    let output = "a".repeat(100);
    assert_eq!(truncate_tool_result(&output, 100), output);
}

#[test]
fn truncate_tool_result_zero_disables() {
    let output = "a".repeat(200_000);
    assert_eq!(truncate_tool_result(&output, 0), output);
}

#[test]
fn truncate_tool_result_truncates_with_marker() {
    let output = "a".repeat(200);
    let result = truncate_tool_result(&output, 100);
    assert!(result.contains("[... "));
    assert!(result.contains("characters truncated ...]\n\n"));
    // Head should be ~2/3 of 100 = 66, tail ~1/3 = 34
    assert!(result.starts_with("aaa"));
    assert!(result.ends_with("aaa"));
    // Result should be shorter than original
    assert!(result.len() < output.len());
}

#[test]
fn truncate_tool_result_preserves_head_tail_ratio() {
    let output: String = (0u32..1000)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    let result = truncate_tool_result(&output, 300);
    // Head = 2/3 of 300 = 200 chars, tail = 100 chars
    // Find the marker
    let marker_start = result.find("[... ").unwrap();
    let marker_end = result.find("characters truncated ...]\n\n").unwrap()
        + "characters truncated ...]\n\n".len();
    let head = &result[..marker_start - 2]; // subtract \n\n
    let tail = &result[marker_end..];
    assert!(
        head.len() >= 190 && head.len() <= 210,
        "head len={}",
        head.len()
    );
    assert!(
        tail.len() >= 90 && tail.len() <= 110,
        "tail len={}",
        tail.len()
    );
}

#[test]
fn truncate_tool_result_utf8_boundary_safety() {
    // Create string with multi-byte chars: each emoji is 4 bytes
    let output = "🦀".repeat(100); // 400 bytes
    // This should not panic even with a limit that falls mid-char
    let result = truncate_tool_result(&output, 50);
    assert!(result.contains("[... "));
    // Verify the result is valid UTF-8 (would panic otherwise)
    let _ = result.len();
}

#[test]
fn truncate_tool_result_very_small_max() {
    let output = "abcdefghijklmnopqrstuvwxyz";
    // With max=5, head=3 tail=2 — result includes marker overhead
    // but should not panic and should contain truncation marker
    let result = truncate_tool_result(output, 5);
    assert!(result.contains("[... "));
    // Head (3 chars) + tail (2 chars) from original should be preserved
    assert!(result.starts_with("abc"));
    assert!(result.ends_with("yz"));
}

// ── truncate_tool_message tests ─────────────────────────────

#[test]
fn truncate_tool_message_preserves_json_structure() {
    use crate::agent::history::truncate_tool_message;
    let big_content = "x".repeat(5000);
    let msg = serde_json::json!({
        "tool_call_id": "call_abc123",
        "content": big_content,
    })
    .to_string();
    let result = truncate_tool_message(&msg, 2000);
    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(parsed["tool_call_id"], "call_abc123");
    assert!(parsed["content"].as_str().unwrap().contains("[... "));
}

#[test]
fn truncate_tool_message_plain_text_fallback() {
    use crate::agent::history::truncate_tool_message;
    let plain = "a".repeat(5000);
    let result = truncate_tool_message(&plain, 2000);
    assert!(result.contains("[... "));
    assert!(result.len() < 5000);
}

#[test]
fn truncate_tool_message_short_passthrough() {
    use crate::agent::history::truncate_tool_message;
    let msg = r#"{"tool_call_id":"call_1","content":"ok"}"#;
    assert_eq!(truncate_tool_message(msg, 2000), msg);
}

// ── estimate_history_tokens tests ─────────────────────────────

#[test]
fn estimate_tokens_empty_history() {
    let history: Vec<ChatMessage> = vec![];
    assert_eq!(estimate_history_tokens(&history), 0);
}

#[test]
fn estimate_tokens_single_message() {
    // 40 chars → 40.div_ceil(4) + 4 = 10 + 4 = 14 tokens
    let msg = "a".repeat(40);
    let history = vec![ChatMessage::user(&msg)];
    let est = estimate_history_tokens(&history);
    assert_eq!(est, 14);
}

#[test]
fn estimate_tokens_multiple_messages() {
    let history = vec![
        ChatMessage::system("system prompt here"), // 18 chars → 18/4=4 +4=8 (div_ceil: 5+4=9)
        ChatMessage::user("hello"),                // 5 chars → 5/4=1 +4=5 (div_ceil: 2+4=6)
        ChatMessage::assistant("world"),           // 5 chars → 5/4=1 +4=5 (div_ceil: 2+4=6)
    ];
    let est = estimate_history_tokens(&history);
    // Each message: content_len.div_ceil(4) + 4
    // 18.div_ceil(4)=5, 5.div_ceil(4)=2, 5.div_ceil(4)=2 → 5+4 + 2+4 + 2+4 = 21
    assert_eq!(est, 21);
}

#[test]
fn estimate_tokens_large_tool_result() {
    let big = "x".repeat(40_000);
    let history = vec![ChatMessage::tool(&big)];
    let est = estimate_history_tokens(&history);
    // 40000.div_ceil(4) + 4 = 10000 + 4 = 10004
    assert_eq!(est, 10_004);
}

use super::*;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[test]
fn scrub_credentials_redacts_bearer_token() {
    let input = "API_KEY=sk-1234567890abcdef; token: 1234567890; password=\"secret123456\"";
    let scrubbed = scrub_credentials(input);
    assert_eq!(
        scrubbed,
        "API_KEY=[REDACTED]; token: [REDACTED]; password=\"[REDACTED]\""
    );
}

#[test]
fn scrub_credentials_redacts_json_api_key() {
    let input = r#"{"api_key": "sk-1234567890", "other": "public"}"#;
    let scrubbed = scrub_credentials(input);
    assert_eq!(scrubbed, r#"{"api_key": "[REDACTED]", "other": "public"}"#);
}

#[tokio::test]
async fn execute_one_tool_does_not_panic_on_utf8_boundary() {
    let call_arguments = (0..600)
        .map(|n| serde_json::json!({ "content": format!("{}：tail", "a".repeat(n)) }))
        .find(|args| {
            let raw = args.to_string();
            raw.len() > 300 && !raw.is_char_boundary(300)
        })
        .expect("should produce a sample whose byte index 300 is not a char boundary");

    let observer = NoopObserver;
    let meta = TurnMeta {
        parent_agent_alias: None,
        agent_alias: None,
        turn_id: "test-turn-id",
        channel_name: "test",
    };
    let result = execute_one_tool(
        "unknown_tool",
        call_arguments,
        None,
        ToolDispatchContext {
            tools_registry: &[],
            activated_tools: None,
            excluded_tools: &[],
            model_switch_callback: None,
        },
        &meta,
        &observer,
        None,
        None,
        None,
    )
    .await;
    assert!(result.is_ok(), "execute_one_tool should not panic or error");

    let outcome = result.unwrap();
    assert!(!outcome.success);
    assert!(outcome.output.contains("Unknown tool: unknown_tool"));
}

#[tokio::test]
async fn execute_one_tool_resolves_unique_activated_tool_suffix() {
    let observer = NoopObserver;
    let invocations = Arc::new(AtomicUsize::new(0));
    let activated = Arc::new(std::sync::Mutex::new(crate::tools::ActivatedToolSet::new()));
    let activated_tool: Arc<dyn Tool> = Arc::new(CountingTool::new(
        "docker-mcp__extract_text",
        Arc::clone(&invocations),
    ));
    activated
        .lock()
        .unwrap()
        .activate("docker-mcp__extract_text".into(), activated_tool);

    let meta = TurnMeta {
        parent_agent_alias: None,
        agent_alias: None,
        turn_id: "test-turn-id",
        channel_name: "test",
    };
    let outcome = execute_one_tool(
        "extract_text",
        serde_json::json!({ "value": "ok" }),
        None,
        ToolDispatchContext {
            tools_registry: &[],
            activated_tools: Some(&activated),
            excluded_tools: &[],
            model_switch_callback: None,
        },
        &meta,
        &observer,
        None,
        None, // receipt_generator
        None, // event_tx
    )
    .await
    .expect("suffix alias should execute the unique activated tool");

    assert!(outcome.success);
    assert_eq!(outcome.output, "counted:ok");
    assert_eq!(invocations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn execute_one_tool_recovers_poisoned_activated_tool_lock() {
    let observer = NoopObserver;
    let invocations = Arc::new(AtomicUsize::new(0));
    let activated = Arc::new(std::sync::Mutex::new(crate::tools::ActivatedToolSet::new()));
    let activated_tool: Arc<dyn Tool> = Arc::new(CountingTool::new(
        "docker-mcp__extract_text",
        Arc::clone(&invocations),
    ));
    activated
        .lock()
        .unwrap()
        .activate("docker-mcp__extract_text".into(), activated_tool);
    let poisoned = Arc::clone(&activated);
    let _ = std::thread::spawn(move || {
        let _guard = poisoned.lock().expect("test mutex should lock");
        panic!("poison activated-tools lock");
    })
    .join();

    let meta = TurnMeta {
        parent_agent_alias: None,
        agent_alias: None,
        turn_id: "test-turn-id",
        channel_name: "test",
    };
    let outcome = execute_one_tool(
        "extract_text",
        serde_json::json!({ "value": "ok" }),
        None,
        ToolDispatchContext {
            tools_registry: &[],
            activated_tools: Some(&activated),
            excluded_tools: &[],
            model_switch_callback: None,
        },
        &meta,
        &observer,
        None,
        None,
        None,
    )
    .await
    .expect("poisoned activated-tools lock should recover for read");

    assert!(outcome.success);
    assert_eq!(outcome.output, "counted:ok");
    assert_eq!(invocations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn execute_one_tool_normalizes_empty_success_output() {
    let observer = NoopObserver;
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(EmptySuccessTool)];

    let meta = TurnMeta {
        parent_agent_alias: None,
        agent_alias: None,
        turn_id: "test-turn-id",
        channel_name: "test",
    };
    let outcome = execute_one_tool(
        "empty_success",
        serde_json::json!({}),
        None,
        ToolDispatchContext {
            tools_registry: &tools,
            activated_tools: None,
            excluded_tools: &[],
            model_switch_callback: None,
        },
        &meta,
        &observer,
        None,
        None, // receipt_generator
        None, // event_tx
    )
    .await
    .expect("empty successful tool output should still execute");

    assert!(outcome.success);
    assert_eq!(outcome.output, "(no output)");
    assert!(outcome.error_reason.is_none());
}

#[tokio::test]
async fn execute_one_tool_keeps_data_path_raw_and_scrubs_only_observer() {
    struct CapturingResults {
        results: std::sync::Mutex<Vec<Option<String>>>,
    }
    impl crate::observability::Observer for CapturingResults {
        fn record_event(&self, event: &crate::observability::ObserverEvent) {
            if let crate::observability::ObserverEvent::ToolCall { result, .. } = event {
                self.results.lock().unwrap().push(result.clone());
            }
        }
        fn record_metric(&self, _metric: &crate::observability::traits::ObserverMetric) {}
        fn name(&self) -> &str {
            "capturing-results"
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn flush(&self) {}
    }

    let observer = CapturingResults {
        results: std::sync::Mutex::new(Vec::new()),
    };
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(CredentialOutputTool)];

    let meta = TurnMeta {
        parent_agent_alias: None,
        agent_alias: None,
        turn_id: "test-turn-id",
        channel_name: "test",
    };
    let outcome = execute_one_tool(
        "credential_output",
        serde_json::json!({}),
        None,
        ToolDispatchContext {
            tools_registry: &tools,
            activated_tools: None,
            excluded_tools: &[],
            model_switch_callback: None,
        },
        &meta,
        &observer,
        None,
        None, // receipt_generator
        None, // event_tx
    )
    .await
    .expect("tool should execute");

    // Data path (fed to the model and HMAC receipts) carries raw bytes.
    assert_eq!(outcome.output, "api_key = \"sk-live-abcd1234efgh5678\"");
    assert!(!outcome.output.contains("[REDACTED]"));

    // Observer (human-facing log/dashboard render) is scrubbed.
    let captured = observer.results.lock().unwrap();
    let result = captured
        .first()
        .and_then(|r| r.as_deref())
        .expect("observer must receive a tool result");
    assert!(result.contains("[REDACTED]"));
    assert!(!result.contains("abcd1234efgh5678"));
}
use crate::observability::NoopObserver;
use tempfile::TempDir;
use zeroclaw_api::model_provider::{ProviderCapabilities, StreamChunk, StreamEvent, StreamOptions};
use zeroclaw_memory::MemoryCategory;
use zeroclaw_providers::ChatResponse;
use zeroclaw_providers::router::{Route, RouterModelProvider};

macro_rules! impl_test_model_provider_attribution {
    ($ty:ty) => {
        impl ::zeroclaw_api::attribution::Attributable for $ty {
            fn role(&self) -> ::zeroclaw_api::attribution::Role {
                ::zeroclaw_api::attribution::Role::Provider(
                    ::zeroclaw_api::attribution::ProviderKind::Model(
                        ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }

            fn alias(&self) -> &str {
                stringify!($ty)
            }
        }
    };
}

struct NonVisionModelProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl ModelProvider for NonVisionModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok("ok".to_string())
    }
}
impl ::zeroclaw_api::attribution::Attributable for NonVisionModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "NonVisionModelProvider"
    }
}

struct VisionModelProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl ModelProvider for VisionModelProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: false,
            vision: true,
            prompt_caching: false,
            extended_thinking: false,
        }
    }

    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok("ok".to_string())
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let marker_count = zeroclaw_providers::multimodal::count_image_markers(request.messages);
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
impl ::zeroclaw_api::attribution::Attributable for VisionModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "VisionModelProvider"
    }
}

struct ScriptedModelProvider {
    responses: Arc<Mutex<VecDeque<ChatResponse>>>,
    capabilities: ProviderCapabilities,
}

impl ScriptedModelProvider {
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

    /// Build a native-tool-calling provider: one turn of structured
    /// `tool_calls`, then a plain-text turn.
    fn from_native_tool_calls(calls: Vec<(&str, &str, &str)>, final_text: &str) -> Self {
        let tool_turn = ChatResponse {
            text: None,
            tool_calls: calls
                .into_iter()
                .map(|(id, name, args)| ToolCall {
                    id: id.to_string(),
                    name: name.to_string(),
                    arguments: args.to_string(),
                    extra_content: None,
                })
                .collect(),
            usage: None,
            reasoning_content: None,
        };
        let final_turn = ChatResponse {
            text: Some(final_text.to_string()),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        };
        let capabilities = ProviderCapabilities {
            native_tool_calling: true,
            ..ProviderCapabilities::default()
        };
        Self {
            responses: Arc::new(Mutex::new(VecDeque::from(vec![tool_turn, final_turn]))),
            capabilities,
        }
    }
}

#[async_trait]
impl ModelProvider for ScriptedModelProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }

    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        anyhow::bail!("chat_with_system should not be used in scripted model_provider tests");
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        let mut responses = self
            .responses
            .lock()
            .expect("responses lock should be valid");
        responses
            .pop_front()
            .ok_or_else(|| anyhow::Error::msg("scripted model_provider exhausted responses"))
    }
}
impl ::zeroclaw_api::attribution::Attributable for ScriptedModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "ScriptedModelProvider"
    }
}

struct RecordingModelProvider {
    requests: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
    capabilities: ProviderCapabilities,
}

impl RecordingModelProvider {
    fn new() -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            capabilities: ProviderCapabilities::default(),
        }
    }

    fn with_vision_support(mut self) -> Self {
        self.capabilities.vision = true;
        self
    }
}

#[async_trait]
impl ModelProvider for RecordingModelProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }

    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        anyhow::bail!("chat_with_system should not be used in recording provider tests");
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        self.requests
            .lock()
            .expect("requests lock should be valid")
            .push(request.messages.to_vec());
        Ok(ChatResponse {
            text: Some("done".to_string()),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        })
    }
}
impl ::zeroclaw_api::attribution::Attributable for RecordingModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "RecordingModelProvider"
    }
}

struct StreamingScriptedModelProvider {
    responses: Arc<Mutex<VecDeque<String>>>,
    stream_calls: Arc<AtomicUsize>,
    chat_calls: Arc<AtomicUsize>,
}

impl StreamingScriptedModelProvider {
    fn from_text_responses(responses: Vec<&str>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(
                responses.into_iter().map(ToString::to_string).collect(),
            )),
            stream_calls: Arc::new(AtomicUsize::new(0)),
            chat_calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl ModelProvider for StreamingScriptedModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        anyhow::bail!(
            "chat_with_system should not be used in streaming scripted model_provider tests"
        );
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        self.chat_calls.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("chat should not be called when streaming succeeds")
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn stream_chat_with_history(
        &self,
        _messages: &[ChatMessage],
        _model: &str,
        _temperature: Option<f64>,
        options: StreamOptions,
    ) -> futures_util::stream::BoxStream<
        'static,
        zeroclaw_providers::traits::StreamResult<StreamChunk>,
    > {
        self.stream_calls.fetch_add(1, Ordering::SeqCst);
        if !options.enabled {
            return Box::pin(futures_util::stream::empty());
        }

        let response = self
            .responses
            .lock()
            .expect("responses lock should be valid")
            .pop_front()
            .unwrap_or_default();

        Box::pin(futures_util::stream::iter(vec![
            Ok(StreamChunk::delta(response)),
            Ok(StreamChunk::final_chunk()),
        ]))
    }
}
impl ::zeroclaw_api::attribution::Attributable for StreamingScriptedModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "StreamingScriptedModelProvider"
    }
}

enum NativeStreamTurn {
    ToolCall(ToolCall),
    Text(String),
    TextChunks(Vec<String>),
    /// Emit a single text delta with associated reasoning content. Used by
    /// regression tests for(DeepSeek V4 thinking-mode replay).
    TextWithReasoning {
        text: String,
        reasoning: String,
    },
    NarrationThenToolCall {
        narration_chunks: Vec<String>,
        tool_call: ToolCall,
    },
    ToolCallThenNarration {
        tool_call: ToolCall,
        narration_chunks: Vec<String>,
    },
}

struct StreamingNativeToolEventModelProvider {
    turns: Arc<Mutex<VecDeque<NativeStreamTurn>>>,
    stream_calls: Arc<AtomicUsize>,
    stream_tool_requests: Arc<AtomicUsize>,
    chat_calls: Arc<AtomicUsize>,
}

impl StreamingNativeToolEventModelProvider {
    fn with_turns(turns: Vec<NativeStreamTurn>) -> Self {
        Self {
            turns: Arc::new(Mutex::new(turns.into())),
            stream_calls: Arc::new(AtomicUsize::new(0)),
            stream_tool_requests: Arc::new(AtomicUsize::new(0)),
            chat_calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl ModelProvider for StreamingNativeToolEventModelProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: true,
            vision: false,
            prompt_caching: false,
            extended_thinking: false,
        }
    }

    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        anyhow::bail!(
            "chat_with_system should not be used in streaming native tool event model_provider tests"
        );
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        self.chat_calls.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("chat should not be called when native streaming events succeed")
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_streaming_tool_events(&self) -> bool {
        true
    }

    fn stream_chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
        options: StreamOptions,
    ) -> futures_util::stream::BoxStream<
        'static,
        zeroclaw_providers::traits::StreamResult<StreamEvent>,
    > {
        self.stream_calls.fetch_add(1, Ordering::SeqCst);
        if request.tools.is_some_and(|tools| !tools.is_empty()) {
            self.stream_tool_requests.fetch_add(1, Ordering::SeqCst);
        }
        if !options.enabled {
            return Box::pin(futures_util::stream::empty());
        }

        let turn = self
            .turns
            .lock()
            .expect("turns lock should be valid")
            .pop_front()
            .expect("streaming turns should have scripted output");
        match turn {
            NativeStreamTurn::ToolCall(tool_call) => Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::ToolCall(tool_call)),
                Ok(StreamEvent::Final),
            ])),
            NativeStreamTurn::Text(text) => Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta(StreamChunk::delta(text))),
                Ok(StreamEvent::Final),
            ])),
            NativeStreamTurn::TextChunks(chunks) => {
                let mut events: Vec<_> = chunks
                    .into_iter()
                    .map(|text| Ok(StreamEvent::TextDelta(StreamChunk::delta(text))))
                    .collect();
                events.push(Ok(StreamEvent::Final));
                Box::pin(futures_util::stream::iter(events))
            }
            NativeStreamTurn::TextWithReasoning { text, reasoning } => {
                Box::pin(futures_util::stream::iter(vec![
                    Ok(StreamEvent::TextDelta(StreamChunk::reasoning(reasoning))),
                    Ok(StreamEvent::TextDelta(StreamChunk::delta(text))),
                    Ok(StreamEvent::Final),
                ]))
            }
            NativeStreamTurn::NarrationThenToolCall {
                narration_chunks,
                tool_call,
            } => {
                let mut events: Vec<_> = narration_chunks
                    .into_iter()
                    .map(|text| Ok(StreamEvent::TextDelta(StreamChunk::delta(text))))
                    .collect();
                events.push(Ok(StreamEvent::ToolCall(tool_call)));
                events.push(Ok(StreamEvent::Final));
                Box::pin(futures_util::stream::iter(events))
            }
            NativeStreamTurn::ToolCallThenNarration {
                tool_call,
                narration_chunks,
            } => {
                let mut events: Vec<_> = vec![Ok(StreamEvent::ToolCall(tool_call))];
                events.extend(
                    narration_chunks
                        .into_iter()
                        .map(|text| Ok(StreamEvent::TextDelta(StreamChunk::delta(text)))),
                );
                events.push(Ok(StreamEvent::Final));
                Box::pin(futures_util::stream::iter(events))
            }
        }
    }
}
impl ::zeroclaw_api::attribution::Attributable for StreamingNativeToolEventModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "StreamingNativeToolEventModelProvider"
    }
}

struct RouteAwareStreamingModelProvider {
    response: String,
    stream_calls: Arc<AtomicUsize>,
    chat_calls: Arc<AtomicUsize>,
    last_model: Arc<Mutex<String>>,
}

impl RouteAwareStreamingModelProvider {
    fn new(response: &str) -> Self {
        Self {
            response: response.to_string(),
            stream_calls: Arc::new(AtomicUsize::new(0)),
            chat_calls: Arc::new(AtomicUsize::new(0)),
            last_model: Arc::new(Mutex::new(String::new())),
        }
    }
}

#[async_trait]
impl ModelProvider for RouteAwareStreamingModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        anyhow::bail!("chat_with_system should not be used in route-aware stream tests");
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        self.chat_calls.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("chat should not be called when routed streaming succeeds")
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn stream_chat_with_history(
        &self,
        _messages: &[ChatMessage],
        model: &str,
        _temperature: Option<f64>,
        options: StreamOptions,
    ) -> futures_util::stream::BoxStream<
        'static,
        zeroclaw_providers::traits::StreamResult<StreamChunk>,
    > {
        self.stream_calls.fetch_add(1, Ordering::SeqCst);
        *self
            .last_model
            .lock()
            .expect("last_model lock should be valid") = model.to_string();
        if !options.enabled {
            return Box::pin(futures_util::stream::empty());
        }

        Box::pin(futures_util::stream::iter(vec![
            Ok(StreamChunk::delta(self.response.clone())),
            Ok(StreamChunk::final_chunk()),
        ]))
    }
}
impl ::zeroclaw_api::attribution::Attributable for RouteAwareStreamingModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "RouteAwareStreamingModelProvider"
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

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<crate::tools::ToolResult> {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        let value = args
            .get("value")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        Ok(crate::tools::ToolResult {
            success: true,
            output: format!("counted:{value}").into(),
            error: None,
        })
    }
}

struct ApprovingChannel {
    approval_requests: Arc<AtomicUsize>,
}

impl ApprovingChannel {
    fn new(approval_requests: Arc<AtomicUsize>) -> Self {
        Self { approval_requests }
    }
}

impl ::zeroclaw_api::attribution::Attributable for ApprovingChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::AcpChannel,
        )
    }

    fn alias(&self) -> &str {
        "approving-test"
    }
}

#[async_trait]
impl Channel for ApprovingChannel {
    fn name(&self) -> &str {
        "approving-test"
    }

    async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn request_approval(
        &self,
        _recipient: &str,
        _request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        self.approval_requests.fetch_add(1, Ordering::SeqCst);
        Ok(Some(ChannelApprovalResponse::Approve))
    }
}

/// A channel that always answers `Deny`, with a configurable provenance.
///
/// This is the exact ambiguity the gate has to resolve: an operator tapping
/// "Deny" and a channel synthesizing a deny because nobody answered are
/// indistinguishable on the wire — both are `Some(Deny)` with no decider.
/// Only [`ApprovalSource`] separates them.
struct SourcedDenyChannel {
    source: ::zeroclaw_api::channel::ApprovalSource,
}

impl ::zeroclaw_api::attribution::Attributable for SourcedDenyChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::AcpChannel,
        )
    }

    fn alias(&self) -> &str {
        "sourced-deny-test"
    }
}

#[async_trait]
impl Channel for SourcedDenyChannel {
    fn name(&self) -> &str {
        "sourced-deny-test"
    }

    async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn request_approval_attributed(
        &self,
        _recipient: &str,
        _request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<::zeroclaw_api::channel::AttributedApprovalResponse>> {
        Ok(Some(
            ::zeroclaw_api::channel::AttributedApprovalResponse::from_runtime(
                ChannelApprovalResponse::Deny,
                self.source,
            ),
        ))
    }
}

/// Drive one denied tool call through the gate and return the
/// `[Tool results]` text the MODEL would see.
async fn tool_results_for_denying_channel(
    source: ::zeroclaw_api::channel::ApprovalSource,
) -> String {
    tool_results_for_denying_channel_with_store(source, None)
        .await
        .0
}

/// Like [`tool_results_for_denying_channel`], optionally persisting approval
/// audit rows under `data_dir`. Returns the tool results and the turn id.
async fn tool_results_for_denying_channel_with_store(
    source: ::zeroclaw_api::channel::ApprovalSource,
    data_dir: Option<&std::path::Path>,
) -> (String, String) {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let write_call = r#"<tool_call>
{"name":"file_write","arguments":{"path":"a.txt","content":"x"}}
</tool_call>"#;
    let model_provider = ScriptedModelProvider::from_text_responses(vec![write_call, "done"]);

    let invocations = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
        "file_write",
        Arc::clone(&invocations),
    ))];

    let channel = SourcedDenyChannel { source };
    let approval_mgr = ApprovalManager::for_non_interactive_backchannel(
        &zeroclaw_config::schema::RiskProfileConfig::default(),
    );
    let approval_mgr = match data_dir {
        Some(dir) => approval_mgr.with_store_at(dir),
        None => approval_mgr,
    };
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("write a file"),
    ];
    let observer = NoopObserver;

    let _ = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: Some(&approval_mgr),
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "acp",
        channel_reply_target: Some("operator"),
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: Some(&channel),
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await;

    assert_eq!(
        invocations.load(Ordering::SeqCst),
        0,
        "a denied tool must not execute"
    );

    let content = history
        .iter()
        .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
        .expect("tool results message should be present")
        .content
        .clone();
    (content, turn_id)
}

/// The gate-level half of the contract: an OPERATOR's deny still reads as a
/// user decision. Without this the fix could "pass" by never saying
/// "Denied by user." at all, which would lose real information.
#[tokio::test]
async fn operator_sourced_deny_is_still_reported_as_a_user_denial() {
    let content =
        tool_results_for_denying_channel(::zeroclaw_api::channel::ApprovalSource::Operator).await;
    assert!(
        content.contains("Denied by user."),
        "an operator's deny is a real user decision and must say so: {content}"
    );
}

/// The other half: a channel-synthesized deny reaching the gate with
/// runtime provenance must NOT be reported as a user's decision, even
/// though it is byte-identical to one on the `ChannelApprovalResponse` wire.
#[tokio::test]
async fn runtime_sourced_deny_is_not_reported_as_a_user_denial() {
    for source in [
        ::zeroclaw_api::channel::ApprovalSource::TimedOut,
        ::zeroclaw_api::channel::ApprovalSource::Unreachable,
        ::zeroclaw_api::channel::ApprovalSource::Unavailable,
    ] {
        let content = tool_results_for_denying_channel(source).await;
        assert!(
            !content.contains("Denied by user."),
            "{source:?} is a runtime denial, not a user's: {content}"
        );
        assert!(
            content.contains("no operator decision was available"),
            "{source:?} should state that no operator decided: {content}"
        );
    }
}

/// The durable audit trail must not record a runtime fail-closed denial as
/// a human refusal: it is `timed_out` with no approver, while an operator's
/// deny stays `denied`.
#[tokio::test]
async fn runtime_sourced_deny_is_audited_as_timed_out_not_denied() {
    for (source, expected) in [
        (::zeroclaw_api::channel::ApprovalSource::Operator, "denied"),
        (
            ::zeroclaw_api::channel::ApprovalSource::TimedOut,
            "timed_out",
        ),
        (
            ::zeroclaw_api::channel::ApprovalSource::Unreachable,
            "timed_out",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (_, turn_id) =
            tool_results_for_denying_channel_with_store(source, Some(dir.path())).await;
        let store = crate::approval::store::ApprovalStore::open(dir.path(), "audit-read").unwrap();
        let rows = store.audit_for_run(&turn_id).unwrap();
        assert!(
            rows.iter().any(|(_, _, decision)| decision == expected),
            "{source:?} should be audited as {expected}: {rows:?}"
        );
    }
}

struct CredentialOutputTool;

#[async_trait]
impl Tool for CredentialOutputTool {
    fn name(&self) -> &str {
        "credential_output"
    }

    fn description(&self) -> &str {
        "Returns success with a credential-shaped value in its output"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<crate::tools::ToolResult> {
        Ok(crate::tools::ToolResult {
            success: true,
            output: "api_key = \"sk-live-abcd1234efgh5678\"".to_string().into(),
            error: None,
        })
    }
}

struct EmptySuccessTool;

#[async_trait]
impl Tool for EmptySuccessTool {
    fn name(&self) -> &str {
        "empty_success"
    }

    fn description(&self) -> &str {
        "Returns success with no stdout"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<crate::tools::ToolResult> {
        Ok(crate::tools::ToolResult {
            success: true,
            output: crate::tools::ToolOutput::default(),
            error: None,
        })
    }
}

struct RecordingArgsTool {
    name: String,
    recorded_args: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl RecordingArgsTool {
    fn new(name: &str, recorded_args: Arc<Mutex<Vec<serde_json::Value>>>) -> Self {
        Self {
            name: name.to_string(),
            recorded_args,
        }
    }
}

#[async_trait]
impl Tool for RecordingArgsTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Records tool arguments for regression tests"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": { "type": "string" },
                "schedule": { "type": "object" },
                "delivery": { "type": "object" }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<crate::tools::ToolResult> {
        self.recorded_args
            .lock()
            .expect("recorded args lock should be valid")
            .push(args.clone());
        Ok(crate::tools::ToolResult {
            success: true,
            output: args.to_string().into(),
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

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<crate::tools::ToolResult> {
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
            output: format!("ok:{value}").into(),
            error: None,
        })
    }
}

/// A tool that always returns a failure with a given error reason.
struct FailingTool {
    tool_name: String,
    error_reason: String,
}

impl FailingTool {
    fn new(name: &str, error_reason: &str) -> Self {
        Self {
            tool_name: name.to_string(),
            error_reason: error_reason.to_string(),
        }
    }
}

#[async_trait]
impl Tool for FailingTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        "A tool that always fails for testing failure surfacing"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string" }
            }
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<crate::tools::ToolResult> {
        Ok(crate::tools::ToolResult {
            success: false,
            output: crate::tools::ToolOutput::default(),
            error: Some(self.error_reason.clone()),
        })
    }
}

#[tokio::test]
async fn max_iteration_summary_refreshes_committed_task_context() {
    use crate::tools::task_list::TaskOwnerResolver;
    use crate::tools::task_section::{TASKS_REGION_BEGIN, TASKS_REGION_END};
    use zeroclaw_infra::session_sqlite::SqliteSessionBackend;

    struct SummaryTaskProvider {
        scripted: ScriptedModelProvider,
        requests: Mutex<Vec<(Vec<ChatMessage>, bool)>>,
    }

    #[async_trait]
    impl ModelProvider for SummaryTaskProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            self.scripted.capabilities()
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("the tool loop must use chat")
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            model: &str,
            temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            self.requests
                .lock()
                .unwrap()
                .push((request.messages.to_vec(), request.tools.is_some()));
            self.scripted.chat(request, model, temperature).await
        }
    }

    impl zeroclaw_api::attribution::Attributable for SummaryTaskProvider {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Attributable::role(&self.scripted)
        }

        fn alias(&self) -> &str {
            "summary-task-provider"
        }
    }

    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();
    // Production registry/session startup owns initialization; prompt
    // projection only reads the existing store on each model request.
    let _backend = SqliteSessionBackend::new(&data_dir).unwrap();
    let identity = uuid::Uuid::new_v4().to_string();
    let owner: TaskOwnerResolver = {
        let identity = identity.clone();
        let data_dir = data_dir.clone();
        Arc::new(move || Ok((identity.clone(), data_dir.clone())))
    };
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(crate::tools::TaskListTool::new(
        owner.clone(),
        Arc::new(|| Err("no [tasks.reminder_target] configured".to_string())),
    ))];
    let model_provider = SummaryTaskProvider {
        scripted: ScriptedModelProvider::from_native_tool_calls(
            vec![(
                "commit-task",
                "task_list",
                r#"{"expected_revision":0,"ops":[{"op":"add","description":"committed at the iteration cap"}]}"#,
            )],
            "task saved; final summary",
        ),
        requests: Mutex::new(Vec::new()),
    };
    let observer = NoopObserver;
    let mut history = vec![
        ChatMessage::system("Keep the existing identity instructions."),
        ChatMessage::user("Save this commitment."),
    ];
    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: Some(&owner),
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 1,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: Some("task-owner"),
        turn_id: "task-cap-turn",
    })
    .await
    .unwrap();

    assert!(result.contains("task saved; final summary"));
    let requests = model_provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "one tool request plus the final summary");
    assert!(requests[0].1, "first request must offer the real task tool");
    assert!(!requests[1].1, "the final summary must not offer tools");
    let initial_system = &requests[0].0[0].content;
    let summary_system = &requests[1].0[0].content;
    assert!(initial_system.contains("No committed tasks (revision 0)"));
    assert!(summary_system.contains("committed at the iteration cap"));
    assert!(summary_system.contains("expected_revision=1"));
    assert!(!summary_system.contains("No committed tasks"));
    assert!(summary_system.starts_with("Keep the existing identity instructions."));
    assert_eq!(summary_system.matches(TASKS_REGION_BEGIN).count(), 1);
    assert_eq!(summary_system.matches(TASKS_REGION_END).count(), 1);
    let committed = SqliteSessionBackend::new(&data_dir)
        .unwrap()
        .task_list(&identity)
        .unwrap();
    assert_eq!(committed.revision, 1);
    assert_eq!(committed.items.len(), 1);
}

#[tokio::test]
async fn run_tool_call_loop_returns_structured_error_for_non_vision_provider() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let calls = Arc::new(AtomicUsize::new(0));
    let model_provider = NonVisionModelProvider {
        calls: Arc::clone(&calls),
    };

    let mut history = vec![ChatMessage::user(
        "please inspect [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
    )];
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let observer = NoopObserver;

    let err = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 3,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect_err("user image on a non-vision provider should error");

    assert!(err.to_string().contains("provider_capability_error"));
    assert!(err.to_string().contains("capability=vision"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn run_tool_call_loop_skips_oversized_image_payload() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider = RecordingModelProvider::new().with_vision_support();
    let recorded_requests = Arc::clone(&model_provider.requests);

    let oversized_payload = STANDARD.encode(vec![0_u8; (1024 * 1024) + 1]);
    let mut history = vec![ChatMessage::user(format!(
        "[IMAGE:data:image/png;base64,{oversized_payload}]"
    ))];

    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let observer = NoopObserver;
    let multimodal = zeroclaw_config::schema::MultimodalConfig {
        max_images: 4,
        max_image_size_mb: 1,
        allow_remote_fetch: false,
        ..Default::default()
    };

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &multimodal,
            config: None,
            max_tool_iterations: 3,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("oversized payload should be skipped and continue as text-only");

    assert_eq!(result, "done");
    let requests = recorded_requests
        .lock()
        .expect("recorded requests lock should be valid");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].len(), 1);
    assert!(
        requests[0][0]
            .content
            .contains("1 attached image(s) could not be loaded")
    );
    assert!(!requests[0][0].content.contains("[IMAGE:"));
    assert!(!requests[0][0].content.contains(&oversized_payload));
}

#[tokio::test]
async fn run_tool_call_loop_degrades_carried_over_image_on_non_vision_provider() {
    let model_provider = RecordingModelProvider::new();
    let recorded_requests = Arc::clone(&model_provider.requests);

    // An earlier image turn left its marker in history; the latest user
    // message is plain text.
    let mut history = vec![
        ChatMessage::user("please inspect [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string()),
        ChatMessage::user("what is WAL?".to_string()),
    ];
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let observer = NoopObserver;
    let turn_id = uuid::Uuid::new_v4().to_string();

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 3,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("a carried-over image must not fail a plain-text turn");

    assert_eq!(result, "done");

    // The provider was actually called (no hard capability error) and the
    // carried-over marker was stripped before reaching the text-only model.
    let requests = recorded_requests
        .lock()
        .expect("recorded requests lock should be valid");
    assert_eq!(requests.len(), 1, "exactly one provider call expected");
    let sent_blob = requests[0]
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !sent_blob.contains("[IMAGE:"),
        "carried-over image marker must be stripped, got: {sent_blob}"
    );
    assert!(
        sent_blob.contains("[media attachment]"),
        "stripped marker should become the text placeholder, got: {sent_blob}"
    );
}

#[tokio::test]
async fn run_tool_call_loop_accepts_valid_multimodal_request_flow() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let calls = Arc::new(AtomicUsize::new(0));
    let model_provider = VisionModelProvider {
        calls: Arc::clone(&calls),
    };

    let mut history = vec![ChatMessage::user(
        "Analyze this [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
    )];
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let observer = NoopObserver;

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 3,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("valid multimodal payload should pass");

    assert_eq!(result, "vision-ok");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn run_tool_call_loop_degrades_tool_result_image_for_non_vision_provider() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let calls = Arc::new(AtomicUsize::new(0));
    let model_provider = NonVisionModelProvider {
        calls: Arc::clone(&calls),
    };

    // Marker lives in a tool result, not a user message.
    let mut history = vec![
        ChatMessage::user("inspect the screenshot".to_string()),
        ChatMessage::tool(
            "File: /tmp/x.png\n[IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
        ),
    ];
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let observer = NoopObserver;

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 3,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("text-only fallback should succeed, not abort the turn");

    // Provider was invoked (no hard capability error) and returned text.
    assert_eq!(result, "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn run_tool_call_loop_vision_provider_creation_failure() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let calls = Arc::new(AtomicUsize::new(0));
    let model_provider = NonVisionModelProvider {
        calls: Arc::clone(&calls),
    };

    let mut history = vec![ChatMessage::user(
        "inspect [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
    )];
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let observer = NoopObserver;

    let multimodal = zeroclaw_config::schema::MultimodalConfig {
        vision_model_provider: Some("nonexistent-provider-xyz".to_string()),
        vision_model: Some("some-model".to_string()),
        ..Default::default()
    };

    let err = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &multimodal,
            config: None,
            max_tool_iterations: 3,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect_err("should fail when vision model_provider cannot be created");

    assert!(
        err.to_string()
            .contains("failed to create vision model_provider"),
        "expected creation failure error, got: {}",
        err
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn run_tool_call_loop_no_images_uses_default_provider() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider = ScriptedModelProvider::from_text_responses(vec!["hello world"]);

    let mut history = vec![ChatMessage::user("just text, no images".to_string())];
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let observer = NoopObserver;

    let multimodal = zeroclaw_config::schema::MultimodalConfig {
        vision_model_provider: Some("nonexistent-provider-xyz".to_string()),
        vision_model: Some("some-model".to_string()),
        ..Default::default()
    };

    // Even though vision_model_provider points to a nonexistent model_provider, this
    // should succeed because there are no image markers to trigger routing.
    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "scripted",
                model: "scripted-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &multimodal,
            config: None,
            max_tool_iterations: 3,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("text-only messages should succeed with default model_provider");

    assert_eq!(result, "hello world");
}

#[tokio::test]
async fn run_tool_call_loop_ingress_envelope_preserves_normal_turn() {
    async fn run_with(ctx: IngressContext) -> String {
        let model_provider = ScriptedModelProvider::from_text_responses(vec!["identical answer"]);
        let mut history = vec![ChatMessage::user("hello".to_string())];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;
        let turn_id = uuid::Uuid::new_v4().to_string();

        run_tool_call_loop(ToolLoop {
            parent_agent_alias: None,
            exec: ResolvedAgentExecution {
                task_owner: None,
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "scripted",
                    model: "scripted-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                config: None,
                max_tool_iterations: 3,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                keep_tool_context_turns: 2,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            memory: None,
            ingress: ctx,
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("ingress metadata must preserve a tool-free turn without memory injection")
    }

    let internal = run_with(IngressContext::sub_turn()).await;
    let external = run_with(IngressContext {
        message_id: Some("telegram-9001".to_string()),
        source_class: zeroclaw_api::ingress::SourceClass::External,
        sender: Some("external-peer".to_string()),
        transport: zeroclaw_api::ingress::Transport::Channel {
            kind: "telegram".to_string(),
            alias: "test-channel".to_string(),
        },
        trust: zeroclaw_api::ingress::TrustClass::Untrusted,
        origin: zeroclaw_api::ingress::TurnOrigin::Channel,
    })
    .await;

    assert_eq!(internal, "identical answer");
    assert_eq!(
        internal, external,
        "ingress metadata must not change a tool-free turn without memory injection"
    );
}

/// Memory backend whose `recall` returns one Core entry. Used to exercise
/// the engine's unified memory-context injection wiring.
struct StaticRecallMemory;

#[async_trait]
impl zeroclaw_memory::Memory for StaticRecallMemory {
    fn name(&self) -> &str {
        "static-recall"
    }
    async fn store(
        &self,
        _key: &str,
        _content: &str,
        _category: MemoryCategory,
        _session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn recall(
        &self,
        _query: &str,
        _limit: usize,
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
        Ok(vec![zeroclaw_memory::MemoryEntry {
            id: "1".into(),
            key: "remembered".into(),
            content: "the server is prod-3".into(),
            category: MemoryCategory::Core,
            timestamp: chrono::Utc::now().to_rfc3339(),
            session_id: None,
            score: None,
            namespace: "default".into(),
            importance: None,
            superseded_by: None,
            kind: None,
            pinned: false,
            tenant_id: None,
            agent_alias: None,
            agent_id: None,
        }])
    }
    async fn get(&self, _key: &str) -> anyhow::Result<Option<zeroclaw_memory::MemoryEntry>> {
        Ok(None)
    }
    async fn list(
        &self,
        _category: Option<&MemoryCategory>,
        _session_id: Option<&str>,
    ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
        Ok(vec![])
    }
    async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
        Ok(true)
    }
    async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
        Ok(true)
    }
    async fn count(&self) -> anyhow::Result<usize> {
        Ok(1)
    }
    async fn health_check(&self) -> bool {
        true
    }
    async fn store_with_agent(
        &self,
        _key: &str,
        _content: &str,
        _category: MemoryCategory,
        _session_id: Option<&str>,
        _namespace: Option<&str>,
        _importance: Option<f64>,
        _agent_id: Option<&str>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn recall_for_agents(
        &self,
        _allowed_agent_ids: &[&str],
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
        self.recall(query, limit, session_id, since, until).await
    }
}

impl ::zeroclaw_api::attribution::Attributable for StaticRecallMemory {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Memory(::zeroclaw_api::attribution::MemoryKind::InMemory)
    }
    fn alias(&self) -> &str {
        "StaticRecallMemory"
    }
}

#[tokio::test]
async fn run_tool_call_loop_memory_injection_keys_on_origin() {
    async fn run_case(
        mem: &dyn zeroclaw_memory::Memory,
        origin: zeroclaw_api::ingress::TurnOrigin,
    ) -> (String, Vec<(Option<String>, bool)>) {
        let model_provider = ScriptedModelProvider::from_text_responses(vec!["done"]);
        let mut history = vec![ChatMessage::user("what server?".to_string())];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = RecallCountingObserver::default();
        let turn_id = uuid::Uuid::new_v4().to_string();

        run_tool_call_loop(ToolLoop {
            parent_agent_alias: None,
            exec: ResolvedAgentExecution {
                task_owner: None,
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "scripted",
                    model: "scripted-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                config: None,
                max_tool_iterations: 3,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                keep_tool_context_turns: 2,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            memory: Some(crate::agent::memory_inject::TurnMemory {
                handle: mem,
                query: "what server?".to_string(),
                sessions: vec![Some("session-1".to_string())],
                suppress: false,
                cfg: crate::agent::memory_inject::MemoryInjectConfig::default(),
            }),
            ingress: IngressContext::from_origin(origin),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("turn should complete");

        let user_msg = history
            .iter()
            .find(|m| m.role == "user")
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let recalls = observer
            .recalls
            .lock()
            .iter()
            .map(|r| (r.query_summary.clone(), r.success))
            .collect();
        (user_msg, recalls)
    }

    // User-facing origin: preamble injected, one successful recall event.
    let (msg, recalls) = run_case(
        &StaticRecallMemory,
        zeroclaw_api::ingress::TurnOrigin::Interactive,
    )
    .await;
    assert!(msg.starts_with(zeroclaw_memory::MEMORY_CONTEXT_OPEN));
    assert!(msg.contains("- remembered: the server is prod-3"));
    assert!(msg.ends_with("what server?"));
    assert_eq!(recalls.len(), 1);
    assert!(recalls[0].1, "recall event must report success");

    // SubTurn origin: no injection, no recall, no event.
    let (msg, recalls) = run_case(
        &StaticRecallMemory,
        zeroclaw_api::ingress::TurnOrigin::SubTurn,
    )
    .await;
    assert_eq!(msg, "what server?");
    assert!(recalls.is_empty(), "sub-turns must not recall memory");

    // Failing backend: turn proceeds bare, one failure event.
    let (msg, recalls) = run_case(
        &FailingRecallMemory,
        zeroclaw_api::ingress::TurnOrigin::Interactive,
    )
    .await;
    assert_eq!(msg, "what server?");
    assert_eq!(recalls.len(), 1);
    assert!(!recalls[0].1, "recall event must report failure");
}

#[tokio::test]
async fn run_tool_call_loop_vision_provider_without_model_falls_back() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let calls = Arc::new(AtomicUsize::new(0));
    let model_provider = NonVisionModelProvider {
        calls: Arc::clone(&calls),
    };

    let mut history = vec![ChatMessage::user(
        "look [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
    )];
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let observer = NoopObserver;

    // vision_model_provider set but vision_model is None — the code should
    // fall back to the default model. Since the model_provider name is invalid,
    // we just verify the error path references the correct model_provider.
    let multimodal = zeroclaw_config::schema::MultimodalConfig {
        vision_model_provider: Some("nonexistent-provider-xyz".to_string()),
        vision_model: None,
        ..Default::default()
    };

    let err = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &multimodal,
            config: None,
            max_tool_iterations: 3,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect_err("should fail due to nonexistent vision model_provider");

    // Verify the routing was attempted (not the generic capability error).
    assert!(
        err.to_string()
            .contains("failed to create vision model_provider"),
        "expected creation failure, got: {}",
        err
    );
}

#[tokio::test]
async fn run_tool_call_loop_empty_image_markers_use_default_provider() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider = ScriptedModelProvider::from_text_responses(vec!["handled"]);

    let mut history = vec![ChatMessage::user(
        "empty marker [IMAGE:] should be ignored".to_string(),
    )];
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let observer = NoopObserver;

    let multimodal = zeroclaw_config::schema::MultimodalConfig {
        vision_model_provider: Some("nonexistent-provider-xyz".to_string()),
        ..Default::default()
    };

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "scripted",
                model: "scripted-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &multimodal,
            config: None,
            max_tool_iterations: 3,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("empty image markers should not trigger vision routing");

    assert_eq!(result, "handled");
}

#[tokio::test]
async fn run_tool_call_loop_multiple_images_trigger_vision_routing() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let calls = Arc::new(AtomicUsize::new(0));
    let model_provider = NonVisionModelProvider {
        calls: Arc::clone(&calls),
    };

    let mut history = vec![ChatMessage::user(
        "two images [IMAGE:data:image/png;base64,aQ==] and [IMAGE:data:image/png;base64,bQ==]"
            .to_string(),
    )];
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let observer = NoopObserver;

    let multimodal = zeroclaw_config::schema::MultimodalConfig {
        vision_model_provider: Some("nonexistent-provider-xyz".to_string()),
        vision_model: Some("llava:7b".to_string()),
        ..Default::default()
    };

    let err = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &multimodal,
            config: None,
            max_tool_iterations: 3,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect_err("should attempt vision model_provider creation for multiple images");

    assert!(
        err.to_string()
            .contains("failed to create vision model_provider"),
        "expected creation failure for multiple images, got: {}",
        err
    );
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
    let approval_cfg = zeroclaw_config::schema::RiskProfileConfig::default();
    let approval_mgr = ApprovalManager::from_risk_profile(&approval_cfg);

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
    let approval_cfg = zeroclaw_config::schema::RiskProfileConfig {
        level: crate::security::AutonomyLevel::Full,
        ..zeroclaw_config::schema::RiskProfileConfig::default()
    };
    let approval_mgr = ApprovalManager::from_risk_profile(&approval_cfg);

    assert!(should_execute_tools_in_parallel(
        &calls,
        Some(&approval_mgr)
    ));
}

#[tokio::test]
async fn run_tool_call_loop_executes_multiple_tools_with_ordered_results() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider = ScriptedModelProvider::from_text_responses(vec![
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

    let approval_cfg = zeroclaw_config::schema::RiskProfileConfig {
        level: crate::security::AutonomyLevel::Full,
        ..zeroclaw_config::schema::RiskProfileConfig::default()
    };
    let approval_mgr = ApprovalManager::from_risk_profile(&approval_cfg);

    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("run tool calls"),
    ];
    let observer = NoopObserver;

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: Some(&approval_mgr),
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "telegram",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("parallel execution should complete");

    assert!(
        result.ends_with("done"),
        "result should end with 'done', got: {result}"
    );
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
async fn run_tool_call_loop_native_emits_tool_message_per_parallel_call() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider = ScriptedModelProvider::from_native_tool_calls(
        vec![
            ("call_a", "delay_a", r#"{"value":"A"}"#),
            ("call_b", "delay_b", r#"{"value":"B"}"#),
            ("call_c", "delay_c", r#"{"value":"C"}"#),
        ],
        "done",
    );

    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn Tool>> = vec![
        Box::new(DelayTool::new(
            "delay_a",
            10,
            Arc::clone(&active),
            Arc::clone(&max_active),
        )),
        Box::new(DelayTool::new(
            "delay_b",
            10,
            Arc::clone(&active),
            Arc::clone(&max_active),
        )),
        Box::new(DelayTool::new(
            "delay_c",
            10,
            Arc::clone(&active),
            Arc::clone(&max_active),
        )),
    ];

    let approval_cfg = zeroclaw_config::schema::RiskProfileConfig {
        level: crate::security::AutonomyLevel::Full,
        ..zeroclaw_config::schema::RiskProfileConfig::default()
    };
    let approval_mgr = ApprovalManager::from_risk_profile(&approval_cfg);

    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("run three tool calls"),
    ];
    let observer = NoopObserver;

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: Some(&approval_mgr),
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: true,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "telegram",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("native parallel execution should complete");

    assert!(result.ends_with("done"), "got: {result}");

    let tool_messages: Vec<&ChatMessage> =
        history.iter().filter(|msg| msg.role == "tool").collect();
    assert_eq!(
        tool_messages.len(),
        3,
        "every parallel native call must yield its own role=tool message, got: {tool_messages:?}"
    );

    for (id, value) in [("call_a", "A"), ("call_b", "B"), ("call_c", "C")] {
        let msg = tool_messages
            .iter()
            .find(|m| m.content.contains(id))
            .unwrap_or_else(|| panic!("missing tool message for {id}: {tool_messages:?}"));
        assert!(
            msg.content.contains(&format!("ok:{value}")),
            "tool_call_id {id} must carry its own output ok:{value}, got: {}",
            msg.content
        );
    }
}

struct CompletesAndSignalsTool {
    completed_tx: tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

#[async_trait]
impl Tool for CompletesAndSignalsTool {
    fn name(&self) -> &str {
        "fast_tool"
    }
    fn description(&self) -> &str {
        "Completes immediately and signals a sibling to cancel the turn"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{}})
    }
    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<crate::tools::ToolResult> {
        if let Some(tx) = self.completed_tx.lock().await.take() {
            let _ = tx.send(());
        }
        Ok(crate::tools::ToolResult {
            success: true,
            output: "fast-done".to_string().into(),
            error: None,
        })
    }
}

struct CancelsTurnTool {
    token: CancellationToken,
    wait_for: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

#[async_trait]
impl Tool for CancelsTurnTool {
    fn name(&self) -> &str {
        "cancel_tool"
    }
    fn description(&self) -> &str {
        "Waits for a sibling to finish, cancels the turn, then never returns"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{}})
    }
    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<crate::tools::ToolResult> {
        if let Some(rx) = self.wait_for.lock().await.take() {
            let _ = rx.await;
        }
        self.token.cancel();
        // The executor's select drops this future once the token fires; the
        // pending await keeps this call from ever returning Ok.
        std::future::pending::<()>().await;
        unreachable!()
    }
}

#[tokio::test]
async fn run_tool_call_loop_parallel_cancel_no_double_terminal_for_completed_call() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider = ScriptedModelProvider::from_native_tool_calls(
        vec![
            ("call_fast", "fast_tool", "{}"),
            ("call_cancel", "cancel_tool", "{}"),
        ],
        "done",
    );

    let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
    let token = CancellationToken::new();
    let tools_registry: Vec<Box<dyn Tool>> = vec![
        Box::new(CompletesAndSignalsTool {
            completed_tx: tokio::sync::Mutex::new(Some(completed_tx)),
        }),
        Box::new(CancelsTurnTool {
            token: token.clone(),
            wait_for: tokio::sync::Mutex::new(Some(completed_rx)),
        }),
    ];

    let approval_cfg = zeroclaw_config::schema::RiskProfileConfig {
        level: crate::security::AutonomyLevel::Full,
        ..zeroclaw_config::schema::RiskProfileConfig::default()
    };
    let approval_mgr = ApprovalManager::from_risk_profile(&approval_cfg);

    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("run two tool calls"),
    ];
    let observer = NoopObserver;
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<zeroclaw_api::agent::TurnEvent>(64);

    let _ = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: Some(&approval_mgr),
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: true,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "telegram",
        channel_reply_target: None,
        cancellation_token: Some(token.clone()),
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: Some(event_tx),
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await;

    drop(tools_registry);
    let mut results_by_id: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    while let Ok(event) = event_rx.try_recv() {
        if let zeroclaw_api::agent::TurnEvent::ToolResult { id, output, .. } = event {
            results_by_id.entry(id).or_default().push(output);
        }
    }

    let fast_results = results_by_id
        .get("call_fast")
        .map(Vec::as_slice)
        .unwrap_or_default();
    assert_eq!(
        fast_results.len(),
        1,
        "completed parallel call must get exactly one terminal ToolResult, got: {fast_results:?}"
    );
    assert_eq!(fast_results[0], "fast-done");

    let cancel_results = results_by_id
        .get("call_cancel")
        .map(Vec::as_slice)
        .unwrap_or_default();
    assert_eq!(
        cancel_results.len(),
        1,
        "cancelled-in-flight call must get exactly one interrupted ToolResult, got: {cancel_results:?}"
    );
    assert_eq!(
        cancel_results[0],
        crate::i18n::get_required_cli_string("turn-tool-interrupted-before-result")
    );
}

#[tokio::test]
async fn run_tool_call_loop_injects_channel_delivery_defaults_for_cron_add() {
    let model_provider = ScriptedModelProvider::from_text_responses(vec![
        r#"<tool_call>
{"name":"cron_add","arguments":{"job_type":"agent","prompt":"remind me later","schedule":{"kind":"every","every_ms":60000}}}
</tool_call>"#,
        "done",
    ]);

    let recorded_args = Arc::new(Mutex::new(Vec::new()));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(RecordingArgsTool::new(
        "cron_add",
        Arc::clone(&recorded_args),
    ))];

    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("schedule a reminder"),
    ];
    let observer = NoopObserver;

    let turn_id = uuid::Uuid::new_v4().to_string();

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "telegram",
        channel_reply_target: Some("chat-42"),
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("cron_add delivery defaults should be injected");

    assert!(
        result.ends_with("done"),
        "result should end with 'done', got: {result}"
    );

    let recorded = recorded_args
        .lock()
        .expect("recorded args lock should be valid");
    let delivery = recorded[0]["delivery"].clone();
    assert_eq!(
        delivery,
        serde_json::json!({
            "mode": "announce",
            "channel": "telegram",
            "to": "chat-42",
        })
    );
}

#[tokio::test]
async fn run_tool_call_loop_preserves_explicit_cron_delivery_none() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider = ScriptedModelProvider::from_text_responses(vec![
        r#"<tool_call>
{"name":"cron_add","arguments":{"job_type":"agent","prompt":"run silently","schedule":{"kind":"every","every_ms":60000},"delivery":{"mode":"none"}}}
</tool_call>"#,
        "done",
    ]);

    let recorded_args = Arc::new(Mutex::new(Vec::new()));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(RecordingArgsTool::new(
        "cron_add",
        Arc::clone(&recorded_args),
    ))];

    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("schedule a quiet cron job"),
    ];
    let observer = NoopObserver;

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "telegram",
        channel_reply_target: Some("chat-42"),
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("explicit delivery mode should be preserved");

    assert!(
        result.ends_with("done"),
        "result should end with 'done', got: {result}"
    );

    let recorded = recorded_args
        .lock()
        .expect("recorded args lock should be valid");
    assert_eq!(recorded[0]["delivery"], serde_json::json!({"mode": "none"}));
}

#[tokio::test]
async fn run_tool_call_loop_deduplicates_repeated_tool_calls() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider = ScriptedModelProvider::from_text_responses(vec![
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

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("loop should finish after deduplicating repeated calls");

    assert!(
        result.ends_with("done"),
        "result should end with 'done', got: {result}"
    );
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
async fn run_tool_call_loop_allows_low_risk_shell_in_non_interactive_mode() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider = ScriptedModelProvider::from_text_responses(vec![
        r#"<tool_call>
{"name":"shell","arguments":{"command":"echo hello"}}
</tool_call>"#,
        "done",
    ]);

    let tmp = TempDir::new().expect("temp dir");
    let security = Arc::new(crate::security::SecurityPolicy {
        autonomy: crate::security::AutonomyLevel::Supervised,
        workspace_dir: tmp.path().to_path_buf(),
        ..crate::security::SecurityPolicy::default()
    });
    let runtime: Arc<dyn crate::platform::RuntimeAdapter> =
        Arc::new(crate::platform::NativeRuntime::new());
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(crate::tools::shell::ShellTool::new(
        security, runtime,
    ))];

    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("run shell"),
    ];
    let observer = NoopObserver;
    let approval_mgr = ApprovalManager::for_non_interactive(
        &zeroclaw_config::schema::RiskProfileConfig::default(),
    );

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: Some(&approval_mgr),
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "telegram",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("non-interactive shell should succeed for low-risk command");

    assert!(
        result.ends_with("done"),
        "result should end with 'done', got: {result}"
    );

    let tool_results = history
        .iter()
        .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
        .expect("tool results message should be present");
    assert!(tool_results.content.contains("hello"));
    assert!(!tool_results.content.contains("Denied by user."));
}

/// An auto-denial in a non-interactive run must not claim a user denied it.
///
/// With no channel able to answer, the gate denies on the runtime's own authority. Reporting
/// that as "Denied by user." is false and actively misleading: it sends the model looking for
/// an operator decision that never happened, and hides the real remedy, which is the agent's
/// `auto_approve` policy.
#[tokio::test]
async fn run_tool_call_loop_reports_unanswerable_approval_without_blaming_the_user() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let write_call = r#"<tool_call>
{"name":"file_write","arguments":{"path":"a.txt","content":"x"}}
</tool_call>"#;
    let model_provider = ScriptedModelProvider::from_text_responses(vec![write_call, "done"]);

    let invocations = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
        "file_write",
        Arc::clone(&invocations),
    ))];

    let approval_mgr = ApprovalManager::for_non_interactive(
        &zeroclaw_config::schema::RiskProfileConfig::default(),
    );
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("write a file"),
    ];
    let observer = NoopObserver;

    let _ = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: Some(&approval_mgr),
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "telegram",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await;

    let tool_results = history
        .iter()
        .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
        .expect("tool results message should be present");
    assert!(
        !tool_results.content.contains("Denied by user."),
        "an unanswerable approval must not be attributed to a user: {}",
        tool_results.content
    );
    assert!(
        tool_results.content.contains("requires approval")
            && tool_results
                .content
                .contains("no operator decision was available"),
        "the denial should state the outcome and that no operator decided: {}",
        tool_results.content
    );
    // The model must not be told how to switch its own approval policy off.
    // `auto_approve` bypasses operator approval for the named tool and
    // `level = "full"` removes approval gates for every tool while dropping
    // workspace-only confinement, so naming either here hands the model an
    // argument for expanding its own privileges in response to an approval
    // channel simply being unavailable. Operators get that guidance from the
    // WARN record's `operator_hint` instead.
    for leak in ["auto_approve", "level = \"full\"", "risk profile"] {
        assert!(
            !tool_results.content.contains(leak),
            "model-visible denial must not prescribe a policy bypass ({leak}): {}",
            tool_results.content
        );
    }
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        0,
        "a denied tool must not execute"
    );
}

#[tokio::test]
async fn run_tool_call_loop_aborts_repeated_prompt_required_shell_before_reprompting() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let repeated_shell_call = r#"<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>"#;
    let model_provider = ScriptedModelProvider::from_text_responses(vec![
        repeated_shell_call,
        repeated_shell_call,
        "should not reach final response",
    ]);

    let invocations = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
        "shell",
        Arc::clone(&invocations),
    ))];

    let approval_requests = Arc::new(AtomicUsize::new(0));
    let channel = ApprovingChannel::new(Arc::clone(&approval_requests));
    let approval_mgr = ApprovalManager::for_non_interactive_backchannel(
        &zeroclaw_config::schema::RiskProfileConfig::default(),
    );
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("repeat shell"),
    ];
    let observer = NoopObserver;
    let knobs = LoopKnobs {
        dedup_enabled: false,
        ..LoopKnobs::default()
    };

    let err = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: Some(&approval_mgr),
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &knobs,
        },
        history: &mut history,
        channel_name: "acp",
        channel_reply_target: Some("operator"),
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: Some(&channel),
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect_err("identical prompt-required shell call should abort before another prompt");

    let err = err.to_string();
    assert!(
        err.contains("repeated prompt-required tool call 'shell'"),
        "unexpected error: {err}"
    );
    assert_eq!(
        approval_requests.load(Ordering::SeqCst),
        1,
        "the repeated shell call should not issue a second approval request"
    );
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        1,
        "the repeated shell call should not execute a second time"
    );
}

#[tokio::test]
async fn run_tool_call_loop_skips_same_round_prompt_required_shell_duplicate_without_reprompting() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider = ScriptedModelProvider::from_text_responses(vec![
        r#"<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>
<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>"#,
        "done",
    ]);

    let invocations = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
        "shell",
        Arc::clone(&invocations),
    ))];

    let approval_requests = Arc::new(AtomicUsize::new(0));
    let channel = ApprovingChannel::new(Arc::clone(&approval_requests));
    let approval_mgr = ApprovalManager::for_non_interactive_backchannel(
        &zeroclaw_config::schema::RiskProfileConfig::default(),
    );
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("repeat shell in one response"),
    ];
    let observer = NoopObserver;

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: Some(&approval_mgr),
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "acp",
        channel_reply_target: Some("operator"),
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: Some(&channel),
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("same-round prompt-required duplicate should use duplicate result path");

    assert!(
        result.ends_with("done"),
        "result should end with 'done', got: {result}"
    );
    assert_eq!(
        approval_requests.load(Ordering::SeqCst),
        1,
        "same-round duplicate should not issue a second approval request"
    );
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        1,
        "same-round duplicate should not execute a second time"
    );

    let tool_results = history
        .iter()
        .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
        .expect("prompt-mode tool result payload should be present");
    assert!(tool_results.content.contains("counted:"));
    assert!(tool_results.content.contains("Skipped duplicate tool call"));
}

#[tokio::test]
async fn run_tool_call_loop_prompt_guard_ignores_same_round_dedup_exempt_tools() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let repeated_shell_call = r#"<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>"#;
    let model_provider = ScriptedModelProvider::from_text_responses(vec![
        repeated_shell_call,
        repeated_shell_call,
        "should not reach final response",
    ]);

    let invocations = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
        "shell",
        Arc::clone(&invocations),
    ))];

    let approval_requests = Arc::new(AtomicUsize::new(0));
    let channel = ApprovingChannel::new(Arc::clone(&approval_requests));
    let approval_mgr = ApprovalManager::for_non_interactive_backchannel(
        &zeroclaw_config::schema::RiskProfileConfig::default(),
    );
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("repeat dedup-exempt shell"),
    ];
    let observer = NoopObserver;
    let dedup_exempt = vec!["shell".to_string()];

    let err = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: Some(&approval_mgr),
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &dedup_exempt,
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "acp",
        channel_reply_target: Some("operator"),
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: Some(&channel),
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect_err("dedup-exempt prompt-required repeats should still abort before reprompting");

    let err = err.to_string();
    assert!(
        err.contains("repeated prompt-required tool call 'shell'"),
        "unexpected error: {err}"
    );
    assert_eq!(
        approval_requests.load(Ordering::SeqCst),
        1,
        "dedup-exempt repeated shell should not issue a second approval request"
    );
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        1,
        "dedup-exempt repeated shell should not execute a second time"
    );
}

#[tokio::test]
async fn run_tool_call_loop_dedup_exempt_allows_repeated_calls() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider = ScriptedModelProvider::from_text_responses(vec![
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

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &exempt,
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("loop should finish with exempt tool executing twice");

    assert!(
        result.ends_with("done"),
        "result should end with 'done', got: {result}"
    );
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
async fn run_tool_call_loop_reentrant_agent_tools_are_dedup_exempt_by_default() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    // The retired `spawn_subagent` left `REENTRANT_AGENT_TOOLS` with the
    // spawn_subagent wall; the V1 `reasoning_subagent` entrypoint is the
    // surviving re-entrant spawn surface this law pins. The name is
    // spelled LITERALLY (not derived from the list) so the test stays an
    // independent pin: if the list is edited, this test must fail until a
    // human reconciles the two.
    assert_eq!(
        crate::tools::REENTRANT_AGENT_TOOLS,
        &["reasoning_subagent"],
        "REENTRANT_AGENT_TOOLS drifted; reconcile this test's literal name"
    );
    let scripted_calls: &'static str = Box::leak(
        format!(
            r#"<tool_call>
{{"name":"reasoning_subagent","arguments":{{"prompt":"same"}}}}
</tool_call>
<tool_call>
{{"name":"reasoning_subagent","arguments":{{"prompt":"same"}}}}
</tool_call>"#
        )
        .into_boxed_str(),
    );
    let model_provider = ScriptedModelProvider::from_text_responses(vec![scripted_calls, "done"]);

    let invocations = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
        "reasoning_subagent",
        Arc::clone(&invocations),
    ))];

    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("fan out two identical subagents"),
    ];
    let observer = NoopObserver;

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("loop should finish running both identical subagent calls");

    assert!(result.ends_with("done"), "got: {result}");
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        2,
        "both identical reentrant spawn-tool calls must execute"
    );
}

#[tokio::test]
async fn run_tool_call_loop_dedup_exempt_only_affects_listed_tools() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider = ScriptedModelProvider::from_text_responses(vec![
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

    let _result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &exempt,
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
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
    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider = ScriptedModelProvider::from_text_responses(vec![
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

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("native fallback id flow should complete");

    assert!(
        result.ends_with("done"),
        "result should end with 'done', got: {result}"
    );
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

#[tokio::test]
async fn run_tool_call_loop_retries_malformed_tool_protocol_without_leaking_json() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let provider = ScriptedModelProvider::from_text_responses(vec![
        r#"{"toolcalls":[{"name":"count_tool","arguments":{"value":"X"}}]}"#,
        "Recovered answer.",
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

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "matrix",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("malformed tool protocol should retry and recover");

    assert_eq!(result, "Recovered answer.");
    assert!(!result.contains("toolcalls"));
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        0,
        "malformed alias payload should not execute as a tool call"
    );
    assert!(
        history
            .iter()
            .any(|msg| msg.role == "user" && msg.content.contains("[Tool call parse error]")),
        "history should include internal parser feedback for the model"
    );
}

#[tokio::test]
async fn run_tool_call_loop_preserves_unknown_function_call_json_with_tools() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let business_json = r#"{"type":"function_call","name":"support_case","arguments":{"id":"A1"}}"#;
    let provider = ScriptedModelProvider::from_text_responses(vec![business_json]);
    let invocations = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
        "count_tool",
        Arc::clone(&invocations),
    ))];
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("return a support case JSON object"),
    ];
    let observer = NoopObserver;

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "matrix",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("business JSON should be returned as normal text");

    assert_eq!(result, business_json);
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        0,
        "business JSON must not execute any runtime tool"
    );
    assert!(
        history
            .iter()
            .all(|msg| !msg.content.contains("[Tool call parse error]")),
        "business JSON must not trigger internal parser feedback"
    );
}

#[tokio::test]
async fn run_tool_call_loop_preserves_malformed_unknown_tool_calls_json_with_tools() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let business_json = r#"{"tool_calls":[{"name":"support_case","arguments":{"id":"A1"}}"#;
    let provider = ScriptedModelProvider::from_text_responses(vec![business_json]);
    let invocations = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
        "count_tool",
        Arc::clone(&invocations),
    ))];
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("return a partial support case JSON object"),
    ];
    let observer = NoopObserver;

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "matrix",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("unknown business JSON should be returned as normal text");

    assert_eq!(result, business_json);
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        0,
        "business JSON must not execute any runtime tool"
    );
    assert!(
        history
            .iter()
            .all(|msg| !msg.content.contains("[Tool call parse error]")),
        "business JSON must not trigger internal parser feedback"
    );
}

#[tokio::test]
async fn run_tool_call_loop_falls_back_after_repeated_malformed_tool_protocol() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let provider = ScriptedModelProvider::from_text_responses(vec![
        r#"{"toolcalls":[{"call_id":"call_1","arguments":{"value":"X"}}]}"#,
        r#"{"toolcalls":[{"call_id":"call_2","arguments":{"value":"Y"}}]}"#,
        r#"{"toolcalls":[{"call_id":"call_3","arguments":{"value":"Z"}}]}"#,
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

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 6,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "matrix",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("malformed tool protocol should return a safe fallback");

    assert_eq!(
        result,
        crate::i18n::get_required_cli_string("channel-runtime-malformed-tool-output")
    );
    assert!(!result.contains("toolcalls"));
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        0,
        "malformed protocol should never be executed as a tool call"
    );
    let feedback_count = history
        .iter()
        .filter(|msg| msg.role == "user" && msg.content.contains("[Tool call parse error]"))
        .count();
    assert_eq!(feedback_count, MAX_MALFORMED_TOOL_PROTOCOL_RETRIES);
}

#[tokio::test]
async fn run_tool_call_loop_streams_toolcalls_reference_json_when_no_tools_are_enabled() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let reference_json = r#"{"toolcalls":[{"name":"count_tool","arguments":{"value":"X"}}]}"#;
    let provider = StreamingScriptedModelProvider::from_text_responses(vec![reference_json]);
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("return a toolcalls reference JSON object"),
    ];
    let observer = NoopObserver;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(16);

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "matrix",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: Some(tx),
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("toolcalls reference JSON should remain visible without tools");

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            visible_deltas.push_str(&text);
        }
    }

    assert_eq!(result, reference_json);
    assert_eq!(visible_deltas, reference_json);
    assert!(
        history
            .iter()
            .all(|msg| !msg.content.contains("[Tool call parse error]")),
        "toolcalls reference JSON must not trigger internal parser feedback"
    );
}

#[tokio::test]
async fn run_tool_call_loop_returns_toolcalls_reference_json_when_no_tools_are_enabled() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let reference_json = r#"{"toolcalls":[{"name":"count_tool","arguments":{"value":"X"}}]}"#;
    let provider = ScriptedModelProvider::from_text_responses(vec![reference_json]);
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("return a toolcalls reference JSON object"),
    ];
    let observer = NoopObserver;

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("toolcalls reference JSON should remain visible without tools");

    assert_eq!(result, reference_json);
    assert!(
        history
            .iter()
            .all(|msg| !msg.content.contains("[Tool call parse error]")),
        "toolcalls reference JSON must not trigger internal parser feedback"
    );
}

#[tokio::test]
async fn run_tool_call_loop_returns_schema_json_array_when_no_tools_are_enabled() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let schema = r#"[{"name":"planner","parameters":{"goal":"string"}}]"#;
    let provider = ScriptedModelProvider::from_text_responses(vec![schema]);
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("return a JSON schema array"),
    ];
    let observer = NoopObserver;

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("schema JSON should remain visible without tools");

    assert_eq!(result, schema);
    assert!(
        history
            .iter()
            .all(|msg| !msg.content.contains("[Tool call parse error]")),
        "plain schema JSON must not trigger internal parser feedback"
    );
}

#[tokio::test]
async fn run_tool_call_loop_returns_tool_calls_audit_json_when_no_tools_are_enabled() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let audit_json = r#"{"tool_calls":[{"id":"case-1","status":"queued","service":"billing"}]}"#;
    let provider = ScriptedModelProvider::from_text_responses(vec![audit_json]);
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("return a tool call audit JSON object"),
    ];
    let observer = NoopObserver;

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("audit JSON should remain visible without tools");

    assert_eq!(result, audit_json);
    assert!(
        history
            .iter()
            .all(|msg| !msg.content.contains("[Tool call parse error]")),
        "business tool_calls JSON must not trigger internal parser feedback"
    );
}

#[tokio::test]
async fn run_tool_call_loop_returns_function_call_reference_json_when_no_tools_are_enabled() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let reference_json =
        r#"{"type":"function_call","name":"support_case","arguments":{"id":"A1"}}"#;
    let provider = ScriptedModelProvider::from_text_responses(vec![reference_json]);
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("return a function_call reference JSON object"),
    ];
    let observer = NoopObserver;

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("reference JSON should remain visible without tools");

    assert_eq!(result, reference_json);
    assert!(
        history
            .iter()
            .all(|msg| !msg.content.contains("[Tool call parse error]")),
        "reference function_call JSON must not trigger internal parser feedback"
    );
}

#[tokio::test]
async fn run_tool_call_loop_returns_tool_call_tag_example_when_no_tools_are_enabled() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let example = r#"<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>
This is an example, not an invocation."#;
    let provider = ScriptedModelProvider::from_text_responses(vec![example]);
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("show a tool_call tag example"),
    ];
    let observer = NoopObserver;

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("tool_call tag examples should remain visible without tools");

    assert_eq!(result, example);
    assert!(
        history
            .iter()
            .all(|msg| !msg.content.contains("[Tool call parse error]")),
        "tool_call tag examples must not trigger internal parser feedback"
    );
}

#[tokio::test]
async fn run_tool_call_loop_streams_tool_call_fenced_example_with_registered_tool() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let example = r#"```tool_call
{"name":"count_tool","arguments":{"value":"X"}}
```
This is an example, not an invocation."#;
    let provider = StreamingScriptedModelProvider::from_text_responses(vec![example]);
    let invocations = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
        "count_tool",
        Arc::clone(&invocations),
    ))];
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("show a registered tool_call fenced example"),
    ];
    let observer = NoopObserver;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(16);

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "matrix",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: Some(tx),
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("registered tool_call fenced examples should remain visible");

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            visible_deltas.push_str(&text);
        }
    }

    assert_eq!(result, example);
    assert_eq!(visible_deltas, example);
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        0,
        "tool-call examples must not execute registered tools"
    );
    assert!(
        history
            .iter()
            .all(|msg| !msg.content.contains("[Tool call parse error]")),
        "tool-call examples must not trigger internal parser feedback"
    );
}

#[tokio::test]
async fn run_tool_call_loop_returns_tool_call_tag_example_with_registered_tool() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let example = r#"<tool_call>
{"name":"count_tool","arguments":{"value":"X"}}
</tool_call>
This is an example, not an invocation."#;
    let provider = ScriptedModelProvider::from_text_responses(vec![example]);
    let invocations = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
        "count_tool",
        Arc::clone(&invocations),
    ))];
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("show a registered tool_call tag example"),
    ];
    let observer = NoopObserver;

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("registered tool_call tag examples should remain visible");

    assert_eq!(result, example);
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        0,
        "tool-call tag examples must not execute registered tools"
    );
}

#[tokio::test]
async fn run_tool_call_loop_retries_tagged_tool_call_with_trailing_text_without_tools() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let leaked = r#"<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>
Done."#;
    let provider = ScriptedModelProvider::from_text_responses(vec![leaked, "Recovered answer."]);
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("run without tools"),
    ];
    let observer = NoopObserver;

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("tagged tool protocol with trailing text should retry and recover");

    assert_eq!(result, "Recovered answer.");
    assert!(!result.contains("<tool_call>"));
    assert!(
        history
            .iter()
            .any(|msg| msg.role == "user" && msg.content.contains("[Tool call parse error]")),
        "tagged tool protocol with trailing text must trigger internal parser feedback"
    );
}

#[tokio::test]
async fn run_tool_call_loop_retries_embedded_fenced_tool_call_without_tools() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let leaked = r#"Let me call it:
```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```
Done."#;
    let provider = ScriptedModelProvider::from_text_responses(vec![leaked, "Recovered answer."]);
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("run without tools"),
    ];
    let observer = NoopObserver;

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "matrix",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("embedded fenced tool protocol should retry and recover");

    assert_eq!(result, "Recovered answer.");
    assert!(!result.contains("```tool_call"));
    assert!(
        history
            .iter()
            .any(|msg| msg.role == "user" && msg.content.contains("[Tool call parse error]")),
        "embedded fenced tool protocol must trigger internal parser feedback"
    );
}

#[tokio::test]
async fn run_tool_call_loop_retries_malformed_tool_protocol_fenced_call_without_tools() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let leaked = r#"```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```"#;
    let provider = ScriptedModelProvider::from_text_responses(vec![leaked, "Recovered answer."]);
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("run without tools"),
    ];
    let observer = NoopObserver;

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("standalone tool_call fence should retry and recover without tools");

    assert_eq!(result, "Recovered answer.");
    assert!(!result.contains("```tool_call"));
    assert!(
        history
            .iter()
            .any(|msg| msg.role == "user" && msg.content.contains("[Tool call parse error]")),
        "standalone tool_call fence must trigger internal parser feedback"
    );
}

#[tokio::test]
async fn run_tool_call_loop_streams_tool_call_fenced_example_when_no_tools_are_enabled() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let example = r#"```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```
This is an example, not an invocation."#;
    let provider = StreamingScriptedModelProvider::from_text_responses(vec![example]);
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("show a tool_call fenced example"),
    ];
    let observer = NoopObserver;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(16);

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "matrix",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: Some(tx),
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("tool_call fenced examples should remain visible without tools");

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            visible_deltas.push_str(&text);
        }
    }

    assert_eq!(result, example);
    assert_eq!(visible_deltas, example);
    assert!(
        history
            .iter()
            .all(|msg| !msg.content.contains("[Tool call parse error]")),
        "tool_call fenced examples must not trigger internal parser feedback"
    );
}

#[tokio::test]
async fn run_tool_call_loop_streams_split_tool_call_fenced_example_when_no_tools_are_enabled() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    struct SplitFencedExampleProvider;
    impl_test_model_provider_attribution!(SplitFencedExampleProvider);

    #[async_trait]
    impl ModelProvider for SplitFencedExampleProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("not used in this test")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("chat should not be called when streaming succeeds")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<StreamEvent>,
        > {
            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta(StreamChunk::delta(
                    "```tool_call\n{\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}}\n```",
                ))),
                Ok(StreamEvent::TextDelta(StreamChunk::delta(
                    "\nThis is an example, not an invocation.",
                ))),
                Ok(StreamEvent::Final),
            ]))
        }
    }

    let example = r#"```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```
This is an example, not an invocation."#;
    let provider = SplitFencedExampleProvider;
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("show a split tool_call fenced example"),
    ];
    let observer = NoopObserver;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(16);

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "matrix",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: Some(tx),
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("split tool_call fenced examples should remain visible without tools");

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            visible_deltas.push_str(&text);
        }
    }

    assert_eq!(result, example);
    assert_eq!(visible_deltas, example);
    assert!(
        history
            .iter()
            .all(|msg| !msg.content.contains("[Tool call parse error]")),
        "split tool_call fenced examples must not trigger internal parser feedback"
    );
}

#[tokio::test]
async fn run_tool_call_loop_streams_json_fenced_tool_protocol_example_when_no_tools_are_enabled() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let example = r#"```json
{"tool_calls":[{"name":"shell","arguments":{"command":"pwd"}}]}
```
This is an example, not an invocation."#;
    let provider = StreamingScriptedModelProvider::from_text_responses(vec![example]);
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("show a JSON tool_calls example"),
    ];
    let observer = NoopObserver;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(16);

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "matrix",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: Some(tx),
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("JSON-fenced tool protocol examples should remain visible without tools");

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            visible_deltas.push_str(&text);
        }
    }

    assert_eq!(result, example);
    assert_eq!(visible_deltas, example);
    assert!(
        history
            .iter()
            .all(|msg| !msg.content.contains("[Tool call parse error]")),
        "JSON-fenced tool protocol examples must not trigger internal parser feedback"
    );
}

#[tokio::test]
async fn run_tool_call_loop_executes_streamed_tool_call_fence_without_draft_leak() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let provider = StreamingScriptedModelProvider::from_text_responses(vec![
        r#"```tool_call
{"name":"count_tool","arguments":{"value":"X"}}
```"#,
        "Final answer.",
    ]);
    let invocations = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
        "count_tool",
        Arc::clone(&invocations),
    ))];
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("use the tool"),
    ];
    let observer = NoopObserver;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(16);

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "matrix",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: Some(tx),
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("streamed fenced tool call should execute and continue");

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            visible_deltas.push_str(&text);
        }
    }

    assert_eq!(result, "Final answer.");
    assert_eq!(invocations.load(Ordering::SeqCst), 1);
    assert_eq!(visible_deltas, "Final answer.");
    assert!(
        !visible_deltas.contains("```tool_call"),
        "streamed fenced tool call must not reach draft updates before execution"
    );
}

#[tokio::test]
async fn run_tool_call_loop_sanitizes_native_tool_call_text_before_display_and_history() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider = ScriptedModelProvider {
        responses: Arc::new(Mutex::new(VecDeque::from(vec![
            ChatResponse {
                text: Some(
                    "<think>private chain of thought</think>Task started. Waiting 30 seconds before checking status."
                        .into(),
                ),
                tool_calls: vec![ToolCall {
                    id: "call_wait".into(),
                    name: "count_tool".into(),
                    arguments: r#"{"value":"A"}"#.into(),
                    extra_content: None,
                }],
                usage: None,
                reasoning_content: Some("provider reasoning".into()),
            },
            ChatResponse {
                text: Some("Final answer".into()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            },
        ]))),
        capabilities: ProviderCapabilities {
            native_tool_calling: true,
            ..ProviderCapabilities::default()
        },
    };

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
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "telegram",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: Some(tx),
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("native tool-call text should be relayed through on_delta");

    let mut deltas: Vec<DraftEvent> = Vec::new();
    while let Some(delta) = rx.recv().await {
        deltas.push(delta);
    }

    assert!(
        deltas
            .iter()
            .any(|delta| matches!(delta, StreamDelta::Text(t) if t == "Task started. Waiting 30 seconds before checking status.\n")),
        "native assistant text should be sanitized and relayed to on_delta"
    );
    assert!(
        deltas
            .iter()
            .any(|delta| matches!(delta, StreamDelta::Status(t) if t.starts_with("\u{1f4ac} Got 1 tool call(s)"))),
        "tool-call progress line should still be relayed"
    );
    assert_eq!(
        result, "Final answer",
        "final delivered result should not include intermediate tool-call narration"
    );
    assert!(!result.contains("private chain of thought"));
    assert!(!result.contains("<think>"));
    assert!(
        deltas.iter().all(|delta| match delta {
            StreamDelta::Status(text) | StreamDelta::Text(text) =>
                !text.contains("private chain of thought") && !text.contains("<think>"),
        }),
        "draft deltas must not expose inline think tags: {deltas:?}"
    );
    let assistant_tool_history = history
        .iter()
        .find(|message| message.content.contains("\"tool_calls\""))
        .expect("native tool-call turn should persist assistant history");
    let parsed: serde_json::Value = serde_json::from_str(&assistant_tool_history.content).unwrap();
    assert_eq!(
        parsed["content"].as_str(),
        Some("Task started. Waiting 30 seconds before checking status.")
    );
    assert_eq!(
        parsed["reasoning_content"].as_str(),
        Some("provider reasoning")
    );
    assert!(
        !assistant_tool_history
            .content
            .contains("private chain of thought")
    );
    assert!(!assistant_tool_history.content.contains("<think>"));
    assert_eq!(invocations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn run_tool_call_loop_consumes_provider_stream_for_final_response() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider =
        StreamingScriptedModelProvider::from_text_responses(vec!["streamed final answer"]);
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("say hi"),
    ];
    let observer = NoopObserver;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(32);

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "telegram",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: Some(tx),
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("streaming model_provider should complete");

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        match delta {
            StreamDelta::Status(_) => {}
            StreamDelta::Text(text) => {
                visible_deltas.push_str(&text);
            }
        }
    }

    assert_eq!(result, "streamed final answer");
    assert_eq!(
        visible_deltas, "streamed final answer",
        "draft should receive upstream deltas once without post-hoc duplication"
    );
    assert_eq!(model_provider.stream_calls.load(Ordering::SeqCst), 1);
    assert_eq!(model_provider.chat_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn run_tool_call_loop_streaming_path_preserves_tool_loop_semantics() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider = StreamingScriptedModelProvider::from_text_responses(vec![
        r#"<tool_call>
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
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(64);

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 5,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "telegram",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: Some(tx),
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("streaming tool loop should execute tool and finish");

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        match delta {
            StreamDelta::Status(_) => {}
            StreamDelta::Text(text) => {
                visible_deltas.push_str(&text);
            }
        }
    }

    assert!(
        result.ends_with("done"),
        "result should end with 'done', got: {result}"
    );
    assert_eq!(invocations.load(Ordering::SeqCst), 1);
    assert_eq!(model_provider.stream_calls.load(Ordering::SeqCst), 2);
    assert_eq!(model_provider.chat_calls.load(Ordering::SeqCst), 0);
    assert_eq!(visible_deltas, "done");
    assert!(
        !visible_deltas.contains("<tool_call"),
        "draft text should not leak streamed tool payload markers"
    );
}

// Gemini 2.5 Flash emits text alongside its XML tool calls: a ``tool_code``
// Python block + hallucinated result + premature prose in the same response
// turn. None of that text should reach the user — only the final clean reply
// (iteration with no tool calls) should be accumulated.
#[tokio::test]
async fn parsed_tool_call_iteration_text_is_suppressed() {
    let gemini_turn1 = concat!(
        "<tool_call>\n",
        "{\"name\":\"count_tool\",\"arguments\":{\"value\":\"A\"}}\n",
        "</tool_call>\n",
        "```tool_code\nprint(count_tool(value='A'))\n```\n",
        "```\n{\"result\": \"counted:ok\"}\n```\n",
        "I already have the answer, it is counted:ok.",
    );
    let provider = ScriptedModelProvider::from_text_responses(vec![gemini_turn1, "done"]);
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

    let turn_id = uuid::Uuid::new_v4().to_string();
    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 5,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "telegram",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        memory: None,
        ingress: zeroclaw_api::ingress::IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("should complete");

    assert_eq!(
        invocations.load(Ordering::SeqCst),
        1,
        "tool should run once"
    );
    assert_eq!(
        result, "done",
        "hallucinated text from tool-call iteration must be suppressed; got: {result:?}"
    );
}

#[tokio::test]
async fn consume_provider_streaming_response_buffers_split_tool_protocol_markers() {
    struct SplitToolProtocolProvider;
    impl_test_model_provider_attribution!(SplitToolProtocolProvider);

    #[async_trait]
    impl ModelProvider for SplitToolProtocolProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("not used in this test")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("not used in this test")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<StreamEvent>,
        > {
            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta(StreamChunk::delta(r#"{"tool"#))),
                Ok(StreamEvent::TextDelta(StreamChunk::delta(
                    r#"calls":[{"name":"count_tool","arguments":{"value":"X"}}]}"#,
                ))),
                Ok(StreamEvent::Final),
            ]))
        }
    }

    let provider = SplitToolProtocolProvider;
    let messages = vec![ChatMessage::user("hi")];
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

    let outcome = consume_provider_streaming_response(
        &provider,
        &messages,
        Some(&[crate::tools::ToolSpec::new(
            "count_tool".to_string(),
            "Count values".to_string(),
            serde_json::json!({"type": "object"}),
        )]),
        &std::collections::HashSet::from(["count_tool".to_string()]),
        "mock-model",
        Some(0.0),
        None,
        Some(&tx),
        None, // event_tx
        false,
    )
    .await
    .expect("streaming should finish");
    drop(tx);

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            visible_deltas.push_str(&text);
        }
    }

    assert!(outcome.response_text.contains("\"toolcalls\""));
    assert_eq!(
        visible_deltas, "",
        "split internal protocol markers must not reach draft updates"
    );
}

#[tokio::test]
async fn consume_provider_streaming_response_buffers_top_level_tool_call_array() {
    struct TopLevelToolArrayProvider;
    impl_test_model_provider_attribution!(TopLevelToolArrayProvider);

    #[async_trait]
    impl ModelProvider for TopLevelToolArrayProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("not used in this test")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("not used in this test")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<StreamEvent>,
        > {
            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta(StreamChunk::delta(
                    r#"[{"name":"count_tool","arguments":{"value":"X"}}]"#,
                ))),
                Ok(StreamEvent::Final),
            ]))
        }
    }

    let provider = TopLevelToolArrayProvider;
    let messages = vec![ChatMessage::user("hi")];
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

    let outcome = consume_provider_streaming_response(
        &provider,
        &messages,
        Some(&[crate::tools::ToolSpec::new(
            "count_tool".to_string(),
            "Count values".to_string(),
            serde_json::json!({"type": "object"}),
        )]),
        &std::collections::HashSet::from(["count_tool".to_string()]),
        "mock-model",
        Some(0.0),
        None,
        Some(&tx),
        None, // event_tx
        false,
    )
    .await
    .expect("streaming should finish");
    drop(tx);

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            visible_deltas.push_str(&text);
        }
    }

    assert!(outcome.response_text.contains("\"name\""));
    assert_eq!(
        visible_deltas, "",
        "top-level tool-call arrays must not reach draft updates"
    );
}

#[tokio::test]
async fn consume_provider_streaming_response_preserves_schema_array_without_tools() {
    let provider = StreamingScriptedModelProvider::from_text_responses(vec![
        r#"[{"name":"planner","parameters":{"goal":"string"}}]"#,
    ]);
    let messages = vec![ChatMessage::user("return a JSON schema array")];
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

    let outcome = consume_provider_streaming_response(
        &provider,
        &messages,
        None,
        &std::collections::HashSet::new(),
        "mock-model",
        Some(0.0),
        None,
        Some(&tx),
        None, // event_tx
        false,
    )
    .await
    .expect("streaming should finish");
    drop(tx);

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            visible_deltas.push_str(&text);
        }
    }

    assert_eq!(
        outcome.response_text,
        r#"[{"name":"planner","parameters":{"goal":"string"}}]"#
    );
    assert_eq!(visible_deltas, outcome.response_text);
}

#[tokio::test]
async fn consume_provider_streaming_response_preserves_unknown_function_call_json_with_tools() {
    let response = r#"{"type":"function_call","name":"support_case","arguments":{"id":"A1"}}"#;
    let provider = StreamingScriptedModelProvider::from_text_responses(vec![response]);
    let messages = vec![ChatMessage::user("return a support case JSON object")];
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

    let outcome = consume_provider_streaming_response(
        &provider,
        &messages,
        Some(&[crate::tools::ToolSpec::new(
            "count_tool".to_string(),
            "Count values".to_string(),
            serde_json::json!({"type": "object"}),
        )]),
        &std::collections::HashSet::from(["count_tool".to_string()]),
        "mock-model",
        Some(0.0),
        None,
        Some(&tx),
        None, // event_tx
        false,
    )
    .await
    .expect("streaming should finish");
    drop(tx);

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            visible_deltas.push_str(&text);
        }
    }

    assert_eq!(outcome.response_text, response);
    assert_eq!(visible_deltas, response);
}

#[tokio::test]
async fn consume_provider_streaming_response_preserves_malformed_unknown_tool_calls_json_with_tools()
 {
    let response = r#"{"tool_calls":[{"name":"support_case","arguments":{"id":"A1"}}"#;
    let provider = StreamingScriptedModelProvider::from_text_responses(vec![response]);
    let messages = vec![ChatMessage::user(
        "return a partial support case JSON object",
    )];
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

    let outcome = consume_provider_streaming_response(
        &provider,
        &messages,
        Some(&[crate::tools::ToolSpec::new(
            "count_tool".to_string(),
            "Count values".to_string(),
            serde_json::json!({"type": "object"}),
        )]),
        &std::collections::HashSet::from(["count_tool".to_string()]),
        "mock-model",
        Some(0.0),
        None,
        Some(&tx),
        None, // event_tx
        false,
    )
    .await
    .expect("streaming should finish");
    drop(tx);

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            visible_deltas.push_str(&text);
        }
    }

    assert_eq!(outcome.response_text, response);
    assert_eq!(visible_deltas, response);
    assert!(
        !outcome.suppressed_protocol,
        "unknown business JSON must not be suppressed as internal protocol"
    );
}

#[tokio::test]
async fn consume_provider_streaming_response_buffers_malformed_tool_protocol_json() {
    struct MalformedToolProtocolProvider;
    impl_test_model_provider_attribution!(MalformedToolProtocolProvider);

    #[async_trait]
    impl ModelProvider for MalformedToolProtocolProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("not used in this test")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("not used in this test")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<StreamEvent>,
        > {
            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta(StreamChunk::delta(r#"{"tool_"#))),
                Ok(StreamEvent::TextDelta(StreamChunk::delta(
                    r#"calls":[{"call_id":"call_1","arguments":{"value":"X"}}]}"#,
                ))),
                Ok(StreamEvent::Final),
            ]))
        }
    }

    let provider = MalformedToolProtocolProvider;
    let messages = vec![ChatMessage::user("hi")];
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

    let outcome = consume_provider_streaming_response(
        &provider,
        &messages,
        None,
        &std::collections::HashSet::new(),
        "mock-model",
        Some(0.0),
        None,
        Some(&tx),
        None, // event_tx
        false,
    )
    .await
    .expect("streaming should finish");
    drop(tx);

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            visible_deltas.push_str(&text);
        }
    }

    assert!(outcome.response_text.contains("\"tool_calls\""));
    assert_eq!(
        visible_deltas, "",
        "malformed internal protocol JSON must not reach draft updates"
    );
}

#[tokio::test]
async fn consume_provider_streaming_response_drops_truncated_protocol_at_finish() {
    struct TruncatedProtocolProvider;
    impl_test_model_provider_attribution!(TruncatedProtocolProvider);

    #[async_trait]
    impl ModelProvider for TruncatedProtocolProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("not used in this test")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("not used in this test")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<StreamEvent>,
        > {
            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta(StreamChunk::delta(
                    r#"{"tool_call_id":"call_1","content":"raw"#,
                ))),
                Ok(StreamEvent::Final),
            ]))
        }
    }

    let provider = TruncatedProtocolProvider;
    let messages = vec![ChatMessage::user("hi")];
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

    let outcome = consume_provider_streaming_response(
        &provider,
        &messages,
        None,
        &std::collections::HashSet::new(),
        "mock-model",
        Some(0.0),
        None,
        Some(&tx),
        None, // event_tx
        false,
    )
    .await
    .expect("streaming should finish");
    drop(tx);

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            visible_deltas.push_str(&text);
        }
    }

    assert!(outcome.response_text.contains("\"tool_call_id\""));
    assert_eq!(
        visible_deltas, "",
        "truncated internal protocol must not be released at stream finish"
    );
}

#[tokio::test]
async fn consume_provider_streaming_response_preserves_json_fenced_tool_protocol_without_tools() {
    struct JsonFencedToolProtocolProvider;
    impl_test_model_provider_attribution!(JsonFencedToolProtocolProvider);

    #[async_trait]
    impl ModelProvider for JsonFencedToolProtocolProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("not used in this test")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("not used in this test")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<StreamEvent>,
        > {
            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta(StreamChunk::delta("```json\n"))),
                Ok(StreamEvent::TextDelta(StreamChunk::delta(
                    r#"{"tool_calls":[{"name":"count_tool","arguments":{"value":"X"}}]}"#,
                ))),
                Ok(StreamEvent::TextDelta(StreamChunk::delta("\n```"))),
                Ok(StreamEvent::Final),
            ]))
        }
    }

    let provider = JsonFencedToolProtocolProvider;
    let messages = vec![ChatMessage::user("hi")];
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

    let outcome = consume_provider_streaming_response(
        &provider,
        &messages,
        None,
        &std::collections::HashSet::new(),
        "mock-model",
        Some(0.0),
        None,
        Some(&tx),
        None, // event_tx
        false,
    )
    .await
    .expect("streaming should finish");
    drop(tx);

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            visible_deltas.push_str(&text);
        }
    }

    assert!(outcome.response_text.contains("\"tool_calls\""));
    assert_eq!(
        visible_deltas, outcome.response_text,
        "json-fenced protocol-shaped JSON should remain visible when no tools are active"
    );
}

#[tokio::test]
async fn consume_provider_streaming_response_buffers_tool_call_fence_with_tools() {
    struct ToolCallFenceProvider;
    impl_test_model_provider_attribution!(ToolCallFenceProvider);

    #[async_trait]
    impl ModelProvider for ToolCallFenceProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("not used in this test")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("not used in this test")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<StreamEvent>,
        > {
            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta(StreamChunk::delta("```tool_call\n"))),
                Ok(StreamEvent::TextDelta(StreamChunk::delta(
                    r#"{"name":"count_tool","arguments":{"value":"X"}}"#,
                ))),
                Ok(StreamEvent::TextDelta(StreamChunk::delta("\n```"))),
                Ok(StreamEvent::Final),
            ]))
        }
    }

    let provider = ToolCallFenceProvider;
    let messages = vec![ChatMessage::user("hi")];
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

    let outcome = consume_provider_streaming_response(
        &provider,
        &messages,
        Some(&[crate::tools::ToolSpec::new(
            "count_tool".to_string(),
            "Count values".to_string(),
            serde_json::json!({"type": "object"}),
        )]),
        &std::collections::HashSet::from(["count_tool".to_string()]),
        "mock-model",
        Some(0.0),
        None,
        Some(&tx),
        None, // event_tx
        false,
    )
    .await
    .expect("streaming should finish");
    drop(tx);

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            visible_deltas.push_str(&text);
        }
    }

    assert!(outcome.response_text.contains("```tool_call"));
    assert_eq!(
        visible_deltas, "",
        "streamed tool_call fences with registered tools must not reach draft updates"
    );
}

#[tokio::test]
async fn consume_provider_streaming_response_preserves_plain_prefix_before_protocol_without_tools()
{
    struct PrefixedToolProtocolProvider;
    impl_test_model_provider_attribution!(PrefixedToolProtocolProvider);

    #[async_trait]
    impl ModelProvider for PrefixedToolProtocolProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("not used in this test")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("not used in this test")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<StreamEvent>,
        > {
            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta(StreamChunk::delta(
                    r#"Visible prefix {"toolcalls":[{"name":"count_tool","arguments":{"value":"X"}}]}"#,
                ))),
                Ok(StreamEvent::Final),
            ]))
        }
    }

    let provider = PrefixedToolProtocolProvider;
    let messages = vec![ChatMessage::user("hi")];
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

    let outcome = consume_provider_streaming_response(
        &provider,
        &messages,
        None,
        &std::collections::HashSet::new(),
        "mock-model",
        Some(0.0),
        None,
        Some(&tx),
        None, // event_tx
        false,
    )
    .await
    .expect("streaming should finish");
    drop(tx);

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            visible_deltas.push_str(&text);
        }
    }

    assert!(outcome.response_text.contains("\"toolcalls\""));
    assert_eq!(
        visible_deltas, outcome.response_text,
        "prefixed protocol-shaped JSON should remain visible when no tools are active"
    );
}

#[tokio::test]
async fn consume_provider_streaming_response_preserves_split_protocol_after_plain_prefix_without_tools()
 {
    struct SplitPrefixedToolProtocolProvider;
    impl_test_model_provider_attribution!(SplitPrefixedToolProtocolProvider);

    #[async_trait]
    impl ModelProvider for SplitPrefixedToolProtocolProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("not used in this test")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("not used in this test")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<StreamEvent>,
        > {
            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta(StreamChunk::delta(
                    r#"Visible prefix {"tool"#,
                ))),
                Ok(StreamEvent::TextDelta(StreamChunk::delta(
                    r#"calls":[{"name":"count_tool","arguments":{"value":"X"}}]}"#,
                ))),
                Ok(StreamEvent::Final),
            ]))
        }
    }

    let provider = SplitPrefixedToolProtocolProvider;
    let messages = vec![ChatMessage::user("hi")];
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

    let outcome = consume_provider_streaming_response(
        &provider,
        &messages,
        None,
        &std::collections::HashSet::new(),
        "mock-model",
        Some(0.0),
        None,
        Some(&tx),
        None, // event_tx
        false,
    )
    .await
    .expect("streaming should finish");
    drop(tx);

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            visible_deltas.push_str(&text);
        }
    }

    assert!(outcome.response_text.contains("\"toolcalls\""));
    assert_eq!(
        visible_deltas, outcome.response_text,
        "split prefixed protocol-shaped JSON should remain visible when no tools are active"
    );
}

#[tokio::test]
async fn run_tool_call_loop_streams_native_tool_events_without_chat_fallback() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider = StreamingNativeToolEventModelProvider::with_turns(vec![
        NativeStreamTurn::ToolCall(ToolCall {
            id: "call_native_1".to_string(),
            name: "count_tool".to_string(),
            arguments: r#"{"value":"A"}"#.to_string(),
            extra_content: None,
        }),
        NativeStreamTurn::Text("done".to_string()),
    ]);
    let invocations = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
        "count_tool",
        Arc::clone(&invocations),
    ))];
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("run native tools"),
    ];
    let observer = NoopObserver;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(64);

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 5,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "telegram",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: Some(tx),
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("native streaming events should preserve tool loop semantics");

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        match delta {
            StreamDelta::Status(_) => {}
            StreamDelta::Text(text) => {
                visible_deltas.push_str(&text);
            }
        }
    }

    assert!(
        result.ends_with("done"),
        "result should end with 'done', got: {result}"
    );
    assert_eq!(invocations.load(Ordering::SeqCst), 1);
    assert_eq!(model_provider.stream_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        model_provider.stream_tool_requests.load(Ordering::SeqCst),
        2
    );
    assert_eq!(model_provider.chat_calls.load(Ordering::SeqCst), 0);
    assert_eq!(visible_deltas, "done");
}

#[tokio::test]
async fn run_tool_call_loop_does_not_duplicate_streamed_narration_before_native_tool_call() {
    let narration = "About to check the count.";
    let model_provider = StreamingNativeToolEventModelProvider::with_turns(vec![
        NativeStreamTurn::NarrationThenToolCall {
            narration_chunks: vec!["About to ".to_string(), "check the count.".to_string()],
            tool_call: ToolCall {
                id: "call_narrate_1".to_string(),
                name: "count_tool".to_string(),
                arguments: r#"{"value":"A"}"#.to_string(),
                extra_content: None,
            },
        },
        NativeStreamTurn::Text("done".to_string()),
    ]);
    let invocations = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
        "count_tool",
        Arc::clone(&invocations),
    ))];
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("narrate then run native tool"),
    ];
    let observer = NoopObserver;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(64);

    let turn_id = uuid::Uuid::new_v4().to_string();

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 5,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "telegram",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: Some(tx),
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("narration-then-tool streaming should preserve tool loop semantics");

    let mut accumulated = String::new();
    let mut text_deltas: Vec<String> = Vec::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            accumulated.push_str(&text);
            text_deltas.push(text);
        }
    }

    assert_eq!(invocations.load(Ordering::SeqCst), 1);
    assert!(
        result.ends_with("done"),
        "final response should end with 'done', got: {result}"
    );
    assert_eq!(
        accumulated.matches(narration).count(),
        1,
        "narration must appear exactly once in the accumulated draft; deltas={text_deltas:?} accumulated={accumulated:?}"
    );
    assert!(
        result.contains("done"),
        "final turn must not be truncated; got: {result}"
    );
}

#[tokio::test]
async fn run_tool_call_loop_forwards_native_narration_emitted_after_tool_call() {
    let trailing = "Let me check the count.";
    let model_provider = StreamingNativeToolEventModelProvider::with_turns(vec![
        NativeStreamTurn::ToolCallThenNarration {
            tool_call: ToolCall {
                id: "call_trailing_1".to_string(),
                name: "count_tool".to_string(),
                arguments: r#"{"value":"A"}"#.to_string(),
                extra_content: None,
            },
            narration_chunks: vec!["Let me ".to_string(), "check the count.".to_string()],
        },
        NativeStreamTurn::Text("done".to_string()),
    ]);
    let invocations = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
        "count_tool",
        Arc::clone(&invocations),
    ))];
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("run native tool then narrate"),
    ];
    let observer = NoopObserver;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(64);

    let turn_id = uuid::Uuid::new_v4().to_string();

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 5,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "telegram",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: Some(tx),
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("tool-then-narration streaming should preserve tool loop semantics");

    let mut accumulated = String::new();
    let mut text_deltas: Vec<String> = Vec::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            accumulated.push_str(&text);
            text_deltas.push(text);
        }
    }

    assert_eq!(invocations.load(Ordering::SeqCst), 1);
    assert!(
        accumulated.contains(trailing),
        "narration emitted after the native tool call must reach the user; deltas={text_deltas:?} accumulated={accumulated:?}"
    );
    assert_eq!(
        accumulated.matches(trailing).count(),
        1,
        "post-tool narration must be forwarded exactly once, not dropped then re-sent; deltas={text_deltas:?} accumulated={accumulated:?}"
    );
    assert!(
        result.ends_with("done"),
        "final response should end with 'done', got: {result}"
    );
}

#[tokio::test]
async fn run_tool_call_loop_preserves_guard_withheld_narration_tail_before_tool_call() {
    let narration_full = "Checking <tool";
    let model_provider = StreamingNativeToolEventModelProvider::with_turns(vec![
        NativeStreamTurn::NarrationThenToolCall {
            narration_chunks: vec!["Checking ".to_string(), "<tool".to_string()],
            tool_call: ToolCall {
                id: "call_tail_1".to_string(),
                name: "count_tool".to_string(),
                arguments: r#"{"value":"A"}"#.to_string(),
                extra_content: None,
            },
        },
        NativeStreamTurn::Text("done".to_string()),
    ]);
    let invocations = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
        "count_tool",
        Arc::clone(&invocations),
    ))];
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("narrate with a guard-buffered tail then run a native tool"),
    ];
    let observer = NoopObserver;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(64);

    let turn_id = uuid::Uuid::new_v4().to_string();

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 5,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "telegram",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: Some(tx),
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("guard-withheld narration tail should not break the tool loop");

    let mut accumulated = String::new();
    let mut text_deltas: Vec<String> = Vec::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            accumulated.push_str(&text);
            text_deltas.push(text);
        }
    }

    assert_eq!(invocations.load(Ordering::SeqCst), 1);
    assert!(
        accumulated.contains(narration_full),
        "guard-withheld tail must still reach the draft exactly once; deltas={text_deltas:?} accumulated={accumulated:?}"
    );
    assert_eq!(
        accumulated.matches("<tool").count(),
        1,
        "narration tail must not be duplicated; deltas={text_deltas:?} accumulated={accumulated:?}"
    );
    assert!(
        result.ends_with("done"),
        "final turn must not be truncated; got: {result}"
    );
}

#[tokio::test]
async fn consume_provider_streaming_response_strips_split_think_tags_before_forwarding() {
    let model_provider =
        StreamingNativeToolEventModelProvider::with_turns(vec![NativeStreamTurn::TextChunks(
            vec![
                "<thi".to_string(),
                "nk>private stream reasoning</thi".to_string(),
                "nk>visible answer".to_string(),
            ],
        )]);
    let messages = vec![ChatMessage::user("hi")];
    let tools = [crate::tools::ToolSpec::new(
        "count_tool".to_string(),
        "Count values".to_string(),
        serde_json::json!({"type": "object"}),
    )];
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

    let outcome = consume_provider_streaming_response(
        &model_provider,
        &messages,
        Some(&tools),
        &std::collections::HashSet::from(["count_tool".to_string()]),
        "mock-model",
        Some(0.0),
        None,
        Some(&tx),
        None, // event_tx
        true,
    )
    .await
    .expect("streaming should finish");
    drop(tx);

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Text(text) = delta {
            visible_deltas.push_str(&text);
        }
    }

    assert_eq!(outcome.response_text, "visible answer");
    assert_eq!(visible_deltas, "visible answer");
    assert!(!outcome.response_text.contains("private stream reasoning"));
    assert!(!outcome.response_text.contains("<think>"));
    assert!(!visible_deltas.contains("private stream reasoning"));
    assert!(!visible_deltas.contains("<think>"));
}

#[tokio::test]
async fn run_tool_call_loop_routed_streaming_uses_live_provider_deltas_once() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let default_model_provider = RouteAwareStreamingModelProvider::new("default answer");
    let default_stream_calls = Arc::clone(&default_model_provider.stream_calls);
    let default_chat_calls = Arc::clone(&default_model_provider.chat_calls);

    let routed_model_provider = RouteAwareStreamingModelProvider::new("routed streamed answer");
    let routed_stream_calls = Arc::clone(&routed_model_provider.stream_calls);
    let routed_chat_calls = Arc::clone(&routed_model_provider.chat_calls);
    let routed_last_model = Arc::clone(&routed_model_provider.last_model);

    let router = RouterModelProvider::new(
        "test",
        vec![
            ("default".to_string(), Box::new(default_model_provider)),
            ("fast".to_string(), Box::new(routed_model_provider)),
        ],
        vec![(
            "fast".to_string(),
            Route {
                provider_name: "fast".to_string(),
                model: "routed-model".to_string(),
            },
        )],
    );

    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("say hi"),
    ];
    let observer = NoopObserver;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(32);

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &router,
                provider_name: "router",
                model: "hint:fast",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "telegram",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: Some(tx),
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("routed streaming model_provider should complete");

    let mut visible_deltas = String::new();
    while let Some(delta) = rx.recv().await {
        match delta {
            StreamDelta::Status(_) => {}
            StreamDelta::Text(text) => {
                visible_deltas.push_str(&text);
            }
        }
    }

    assert_eq!(result, "routed streamed answer");
    assert_eq!(
        visible_deltas, "routed streamed answer",
        "routed draft should receive upstream deltas once without post-hoc duplication"
    );
    assert_eq!(default_stream_calls.load(Ordering::SeqCst), 0);
    assert_eq!(routed_stream_calls.load(Ordering::SeqCst), 1);
    assert_eq!(default_chat_calls.load(Ordering::SeqCst), 0);
    assert_eq!(routed_chat_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        routed_last_model
            .lock()
            .expect("routed_last_model lock should be valid")
            .as_str(),
        "routed-model"
    );
}

#[test]
fn agent_turn_executes_activated_tool_from_wrapper() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime should initialize");

    runtime.block_on(async {
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"pixel__get_api_health","arguments":{"value":"ok"}}
</tool_call>"#,
            "done",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let activated = Arc::new(std::sync::Mutex::new(crate::tools::ActivatedToolSet::new()));
        let activated_tool: Arc<dyn Tool> = Arc::new(CountingTool::new(
            "pixel__get_api_health",
            Arc::clone(&invocations),
        ));
        activated
            .lock()
            .unwrap()
            .activate("pixel__get_api_health".into(), activated_tool);

        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("use the activated MCP tool"),
        ];
        let observer = NoopObserver;

        let result = agent_turn(
            None,
            &model_provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            Some(0.0),
            true,
            "daemon",
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            4,
            None,
            &[],
            &[],
            Some(&activated),
            None,
            false,
            false, // parallel_tools
            0,     // max_tool_result_chars: disabled for test
            0,     // context_token_budget: disabled for test
            None,  // channel
            TurnOrigin::SubTurn,
            None,
            None, // agent_alias: not under test here
            None, // turn_id: self-minted
            None,
        )
        .await
        .expect("wrapper path should execute activated tools");

        assert!(
            result.ends_with("done"),
            "result should end with 'done', got: {result}"
        );
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn agent_turn_strict_tool_parsing_ignores_activated_tool_text_from_wrapper() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime should initialize");

    runtime.block_on(async {
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
            r#"<think>private reasoning</think>
<tool_call>
{"name":"pixel__get_api_health","arguments":{"value":"ignored"}}
</tool_call>"#,
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let activated = Arc::new(std::sync::Mutex::new(crate::tools::ActivatedToolSet::new()));
        let activated_tool: Arc<dyn Tool> = Arc::new(CountingTool::new(
            "pixel__get_api_health",
            Arc::clone(&invocations),
        ));
        activated
            .lock()
            .unwrap()
            .activate("pixel__get_api_health".into(), activated_tool);

        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("do not infer activated tool calls from text"),
        ];
        let observer = NoopObserver;

        let result = agent_turn(
            None,
            &model_provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            Some(0.0),
            true,
            "daemon",
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            4,
            None,
            &[],
            &[],
            Some(&activated),
            None,
            true,
            false, // parallel_tools
            0,     // max_tool_result_chars: disabled for test
            0,     // context_token_budget: disabled for test
            None,  // channel
            TurnOrigin::SubTurn,
            None,
            None, // agent_alias: not under test here
            None, // turn_id: self-minted
            None,
        )
        .await
        .expect("strict wrapper path should preserve fallback-looking text");

        assert_eq!(invocations.load(Ordering::SeqCst), 0);
        assert!(
            result.contains("<tool_call>"),
            "strict parser should return fallback-looking text, got: {result}"
        );
        assert!(
            !result.contains("private reasoning"),
            "strict parser should still strip think tags from final text, got: {result}"
        );
    });
}

// ── Regression tests for trimming-budget forwarding through agent_turn ────

/// A mock tool that produces a configurable-length output string for
/// testing that `max_tool_result_chars` is forwarded through
/// `agent_turn` to `run_tool_call_loop`.
struct VerboseTool {
    name: String,
    output_len: usize,
    invocations: Arc<AtomicUsize>,
}

impl VerboseTool {
    fn new(name: &str, output_len: usize, invocations: Arc<AtomicUsize>) -> Self {
        Self {
            name: name.to_string(),
            output_len,
            invocations,
        }
    }
}

#[async_trait]
impl Tool for VerboseTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Returns a long output for trimming-budget regression tests"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "value": { "type": "string" }
            }
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<crate::tools::ToolResult> {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        let output = format!(
            "verbose-start-{}-{}",
            self.name,
            "X".repeat(self.output_len)
        );
        Ok(crate::tools::ToolResult {
            success: true,
            output: output.into(),
            error: None,
        })
    }
}

#[test]
fn agent_turn_forwards_max_tool_result_chars_to_truncate_tool_result() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime should initialize");

    runtime.block_on(async {
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"verbose_checker","arguments":{"value":"check"}}
</tool_call>"#,
            "done",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let verbose_tool: Box<dyn Tool> = Box::new(VerboseTool::new(
            "verbose_checker",
            500, // produce 500+ chars of output
            Arc::clone(&invocations),
        ));
        let tools_registry: Vec<Box<dyn Tool>> = vec![verbose_tool];
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("check"),
        ];
        let observer = NoopObserver;

        let _result = agent_turn(
            None,
            &model_provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            Some(0.0),
            true,
            "daemon",
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            4,
            None,
            &[],
            &[],
            None,
            None,
            false,
            false,
            100, // max_tool_result_chars: truncate at 100 chars
            0,   // context_token_budget: disabled
            None,
            TurnOrigin::SubTurn,
            None,
            None, // agent_alias: not under test here
            None, // turn_id: self-minted
            None,
        )
        .await
        .expect("agent_turn should complete");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "tool should be called once"
        );

        // The tool result in history should be truncated (contain the
        // truncation marker "...") rather than preserving the full 500+ char output.
        let all_content: String = history
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<&str>>()
            .join(" ");

        assert!(
            !all_content.contains(&"X".repeat(500)),
            "tool result should not contain 500 consecutive X chars when truncated to 100 chars"
        );
    });
}

#[test]
fn agent_turn_with_zero_budget_preserves_full_tool_result() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime should initialize");

    runtime.block_on(async {
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"verbose_checker","arguments":{"value":"check"}}
</tool_call>"#,
            "done",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let verbose_tool: Box<dyn Tool> = Box::new(VerboseTool::new(
            "verbose_checker",
            500,
            Arc::clone(&invocations),
        ));
        let tools_registry: Vec<Box<dyn Tool>> = vec![verbose_tool];
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("check"),
        ];
        let observer = NoopObserver;

        let _result = agent_turn(
            None,
            &model_provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            Some(0.0),
            true,
            "daemon",
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            4,
            None,
            &[],
            &[],
            None,
            None,
            false,
            false,
            0, // max_tool_result_chars: disabled (no truncation)
            0, // context_token_budget: disabled
            None,
            TurnOrigin::SubTurn,
            None,
            None, // agent_alias: not under test here
            None, // turn_id: self-minted
            None,
        )
        .await
        .expect("agent_turn should complete");

        assert_eq!(invocations.load(Ordering::SeqCst), 1);

        // Check that the full output is preserved somewhere in history
        // (tool results may appear under different roles depending on the
        // message format used by run_tool_call_loop).
        let all_content: String = history
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<&str>>()
            .join(" ");

        assert!(
            all_content.contains(&"X".repeat(500)),
            "full tool result should be preserved somewhere in history when max_tool_result_chars=0, \
             history roles: {:?}",
            history.iter().map(|m| m.role.clone()).collect::<Vec<_>>()
        );
    });
}

#[test]
fn resolve_display_text_hides_raw_payload_for_tool_only_turns() {
    let display = resolve_display_text(
        "<tool_call>{\"name\":\"memory_store\"}</tool_call>",
        "",
        true,
        false,
    );
    assert!(display.is_empty());
}

#[test]
fn resolve_display_text_keeps_plain_text_for_tool_turns() {
    let display = resolve_display_text(
        "<tool_call>{\"name\":\"shell\"}</tool_call>",
        "Let me check that.",
        true,
        false,
    );
    assert_eq!(display, "Let me check that.");
}

#[test]
fn resolve_display_text_uses_response_text_for_native_tool_turns() {
    let display = resolve_display_text("Task started.", "", true, true);
    assert_eq!(display, "Task started.");
}

#[test]
fn resolve_display_text_uses_response_text_for_final_turns() {
    let display = resolve_display_text("Final answer", "", false, false);
    assert_eq!(display, "Final answer");
}

#[test]
fn build_tool_instructions_includes_all_tools() {
    use crate::security::SecurityPolicy;
    let security = Arc::new(SecurityPolicy::from_risk_profile(
        &zeroclaw_config::schema::RiskProfileConfig::default(),
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
fn build_tool_instructions_empty_registry_returns_empty() {
    let tools: Vec<Box<dyn Tool>> = vec![];
    let instructions = build_tool_instructions(&tools);

    assert!(instructions.is_empty());
}

#[test]
fn tools_to_openai_format_produces_valid_schema() {
    use crate::security::SecurityPolicy;
    let security = Arc::new(SecurityPolicy::from_risk_profile(
        &zeroclaw_config::schema::RiskProfileConfig::default(),
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
fn autosave_memory_key_has_prefix_and_uniqueness() {
    let key1 = autosave_memory_key("user_msg");
    let key2 = autosave_memory_key("user_msg");

    assert!(key1.starts_with("user_msg_"));
    assert!(key2.starts_with("user_msg_"));
    assert_ne!(key1, key2);
}

#[tokio::test]
async fn autosave_memory_keys_preserve_multiple_turns() {
    if !zeroclaw_memory::TACHI_COMPILED {
        return;
    }
    let tmp = TempDir::new().unwrap();
    let cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "tachi".into(),
        hygiene_enabled: false,
        embedding_provider: "none".into(),
        ..Default::default()
    };
    let mem = zeroclaw_memory::create_memory(&cfg, tmp.path(), None).unwrap();

    let key1 = autosave_memory_key("user_msg");
    let key2 = autosave_memory_key("user_msg");

    mem.store(&key1, "I'm Paul", MemoryCategory::Conversation, None)
        .await
        .unwrap();
    mem.store(&key2, "I'm 45", MemoryCategory::Conversation, None)
        .await
        .unwrap();

    assert_eq!(mem.count().await.unwrap(), 2);

    let recalled = mem.recall("45", 5, None, None, None).await.unwrap();
    assert!(recalled.iter().any(|entry| entry.content.contains("45")));
}

#[test]
fn make_query_summary_redacts_credentials_and_caps_length() {
    // Empty input → None (so observers can distinguish "no query
    // recorded" from "empty query string").
    assert!(make_query_summary("").is_none());

    // Plain query passes through unchanged when within the cap.
    let plain = make_query_summary("hello world").unwrap();
    assert_eq!(plain, "hello world");

    // Credential pattern is scrubbed by `scrub_credentials` before
    // the truncation step. The raw token must not appear in the
    // emitted summary.
    let scrubbed = make_query_summary("connect with api_key: sk-proj-abcdef1234567890zzz").unwrap();
    assert!(
        !scrubbed.contains("sk-proj-abcdef1234567890zzz"),
        "raw credential leaked into query_summary: {scrubbed:?}"
    );

    // Long input is truncated. `truncate_with_ellipsis(s, 200)` keeps up
    // to 200 content chars and appends "..." when it had to truncate,
    // for a total ceiling of 203 chars.
    let long_input = "a".repeat(500);
    let truncated = make_query_summary(&long_input).unwrap();
    assert!(
        truncated.chars().count() <= 203,
        "expected ≤203 chars (200 content + ellipsis), got {}",
        truncated.chars().count()
    );
    assert!(
        truncated.ends_with("..."),
        "expected trailing ellipsis on truncated input, got {truncated:?}"
    );
}

/// Captured `MemoryRecall` event used by the wiring-contract tests below.
struct CapturedRecall {
    query_summary: Option<String>,
    success: bool,
}

/// Counting observer that records every `MemoryRecall` event so the test
/// can assert that the runtime hot path emits the variant once per engine
/// memory-context render (`agent::memory_inject::render_memory_context`).
/// Locks in the wiring contract that motivated the memory-OTel work.
#[derive(Default)]
struct RecallCountingObserver {
    recalls: parking_lot::Mutex<Vec<CapturedRecall>>,
}

impl crate::observability::Observer for RecallCountingObserver {
    fn record_event(&self, event: &crate::observability::ObserverEvent) {
        if let crate::observability::ObserverEvent::MemoryRecall {
            query_summary,
            success,
            ..
        } = event
        {
            self.recalls.lock().push(CapturedRecall {
                query_summary: query_summary.clone(),
                success: *success,
            });
        }
    }

    fn record_metric(&self, _metric: &zeroclaw_api::observability_traits::ObserverMetric) {}

    fn name(&self) -> &str {
        "recall-counting-observer"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Memory backend whose `recall` always returns `Err`. Used to exercise
/// the recall-failure arm of the engine's memory-context render; the
/// runtime swallows the error and injects nothing, but observers must
/// still see a `MemoryRecall { success: false }` event.
struct FailingRecallMemory;

#[async_trait]
impl zeroclaw_memory::Memory for FailingRecallMemory {
    fn name(&self) -> &str {
        "failing-recall"
    }
    async fn store(
        &self,
        _key: &str,
        _content: &str,
        _category: MemoryCategory,
        _session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn recall(
        &self,
        _query: &str,
        _limit: usize,
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
        anyhow::bail!("simulated recall failure")
    }
    async fn get(&self, _key: &str) -> anyhow::Result<Option<zeroclaw_memory::MemoryEntry>> {
        Ok(None)
    }
    async fn list(
        &self,
        _category: Option<&MemoryCategory>,
        _session_id: Option<&str>,
    ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
        Ok(Vec::new())
    }
    async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
        Ok(false)
    }
    async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
        Ok(false)
    }
    async fn count(&self) -> anyhow::Result<usize> {
        Ok(0)
    }
    async fn health_check(&self) -> bool {
        false
    }
    async fn store_with_agent(
        &self,
        _key: &str,
        _content: &str,
        _category: MemoryCategory,
        _session_id: Option<&str>,
        _namespace: Option<&str>,
        _importance: Option<f64>,
        _agent_id: Option<&str>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn recall_for_agents(
        &self,
        _allowed_agent_ids: &[&str],
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
        self.recall(query, limit, session_id, since, until).await
    }
}
impl ::zeroclaw_api::attribution::Attributable for FailingRecallMemory {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Memory(::zeroclaw_api::attribution::MemoryKind::InMemory)
    }
    fn alias(&self) -> &str {
        "FailingRecallMemory"
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Recovery Tests - Tool Call Parsing Edge Cases
// ═══════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════
// Recovery Tests - JSON Extraction
// ═══════════════════════════════════════════════════════════════════════

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
    // byte index 4 is not a char boundary
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
    let mut history: Vec<ChatMessage> = vec![];
    trim_history(&mut history, 10);
    assert!(history.is_empty());
}

#[test]
fn trim_history_system_only() {
    let mut history = vec![ChatMessage::system("system prompt")];
    trim_history(&mut history, 10);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].role, "system");
}

#[test]
fn trim_history_exactly_at_limit() {
    let mut history = vec![
        ChatMessage::system("system"),
        ChatMessage::user("msg 1"),
        ChatMessage::assistant("reply 1"),
    ];
    trim_history(&mut history, 2); // 2 non-system messages = exactly at limit
    assert_eq!(history.len(), 3, "should not trim when exactly at limit");
}

#[test]
fn trim_history_keeps_first_user_anchor_and_recent_tail() {
    // The framing anchor (first user message) must survive trim so the
    // model doesn't start a turn thinking "Continue" is the first thing
    // it ever saw. Middle messages are the ones that get dropped.
    let mut history = vec![
        ChatMessage::system("system"),
        ChatMessage::user("anchor: what's the task"),
        ChatMessage::assistant("middle reply 1"),
        ChatMessage::user("middle user 1"),
        ChatMessage::assistant("middle reply 2"),
        ChatMessage::user("recent user"),
        ChatMessage::assistant("recent reply"),
    ];
    // max_history = 3 → keep anchor + 2 most recent (=3 non-system).
    trim_history(&mut history, 3);
    assert_eq!(history[0].role, "system");
    assert_eq!(
        history[1].content, "anchor: what's the task",
        "first user message (framing anchor) must survive"
    );
    let last = history.last().expect("history not empty");
    assert_eq!(last.content, "recent reply", "tail must be preserved");
}

#[test]
fn trim_history_falls_back_to_tail_when_max_history_is_one() {
    // With max_history=1 there's no room for both anchor and tail; fall
    // back to plain head-drop so we don't produce a degenerate window.
    let mut history = vec![
        ChatMessage::system("system"),
        ChatMessage::user("anchor"),
        ChatMessage::assistant("middle"),
        ChatMessage::user("recent"),
    ];
    trim_history(&mut history, 1);
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].role, "system");
    assert_eq!(history[1].content, "recent");
}

#[test]
fn native_tools_system_prompt_contains_zero_xml() {
    use crate::agent::system_prompt::build_system_prompt_with_mode;

    let workspace = tempdir().unwrap();
    let tool_summaries: Vec<(&str, &str)> = vec![
        ("shell", "Execute shell commands"),
        ("file_read", "Read files"),
    ];

    let system_prompt = build_system_prompt_with_mode(
        workspace.path(),
        "test-model",
        &tool_summaries,
        &[],  // no skills
        None, // no identity config
        None, // no bootstrap_max_chars
        true, // native_tool_specs_present
        zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
        crate::security::AutonomyLevel::default(),
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

    // Positive: native prompt should still contain native-task framing.
    assert!(
        !system_prompt.contains("## Tools"),
        "Native prompt should skip the duplicate tools summary"
    );
    assert!(
        system_prompt.contains("## Your Task"),
        "Native prompt should contain task instructions"
    );
}

#[test]
fn native_tool_specs_prompt_allows_actions_without_text_tools() {
    use crate::agent::system_prompt::build_system_prompt_with_mode;

    let workspace = tempdir().unwrap();
    let provider =
        ScriptedModelProvider::from_text_responses(vec!["ok"]).with_native_tool_support();
    let invocations = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn crate::tools::Tool>> =
        vec![Box::new(CountingTool::new("shell", invocations))];

    let native_tool_specs_present =
        super::native_tool_specs_present_for_turn(&provider, &tools_registry, &[], None)
            .expect("native spec availability should be derivable");
    assert!(native_tool_specs_present);

    let system_prompt = build_system_prompt_with_mode(
        workspace.path(),
        "test-model",
        &[],
        &[],
        None,
        None,
        native_tool_specs_present,
        zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
        crate::security::AutonomyLevel::default(),
    );

    assert!(
        !system_prompt.contains("No tools are available"),
        "Native prompt with effective native specs must not deny tool availability"
    );
    assert!(
        system_prompt.contains("Use tools when the request requires action"),
        "Native prompt with effective native specs should authorize action tool use"
    );
}

#[test]
fn native_capable_provider_with_zero_effective_tools_keeps_no_tools_boundary() {
    use crate::agent::system_prompt::build_system_prompt_with_mode;

    let provider =
        ScriptedModelProvider::from_text_responses(vec!["ok"]).with_native_tool_support();
    let invocations = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn crate::tools::Tool>> =
        vec![Box::new(CountingTool::new("shell", invocations))];
    let excluded_tools = vec!["shell".to_string()];

    let native_tool_specs_present = super::native_tool_specs_present_for_turn(
        &provider,
        &tools_registry,
        &excluded_tools,
        None,
    )
    .expect("native spec availability should be derivable");
    assert!(!native_tool_specs_present);

    let system_prompt = build_system_prompt_with_mode(
        std::path::Path::new("/tmp"),
        "test-model",
        &[],
        &[],
        None,
        None,
        native_tool_specs_present,
        zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
        crate::security::AutonomyLevel::default(),
    );

    assert!(
        system_prompt.contains("No tools are available for this turn"),
        "Native-capable providers with zero effective specs must keep the no-tools boundary"
    );
}

#[test]
fn native_tool_specs_present_signal_uses_effective_specs_not_provider_capability() {
    let provider =
        ScriptedModelProvider::from_text_responses(vec!["ok"]).with_native_tool_support();
    assert!(zeroclaw_providers::ModelProvider::supports_native_tools(
        &provider
    ));

    let no_tools: Vec<Box<dyn crate::tools::Tool>> = Vec::new();
    let native_tool_specs_present =
        super::native_tool_specs_present_for_turn(&provider, &no_tools, &[], None)
            .expect("native spec availability should be derivable");

    assert!(
        !native_tool_specs_present,
        "Provider native-tool capability alone must not imply tools are available"
    );
}

#[test]
fn interactive_turn_system_prompt_uses_effective_dynamic_mcp_specs() {
    use crate::agent::system_prompt::{NATIVE_TOOLS_TASK_FRAMING, NO_TOOLS_TASK_FRAMING};
    use zeroclaw_config::schema::{
        RiskProfileConfig, SkillsPromptInjectionMode, ToolFilterGroup, ToolFilterGroupMode,
    };

    let workspace = tempdir().unwrap();
    let provider =
        ScriptedModelProvider::from_text_responses(vec!["ok"]).with_native_tool_support();
    let invocations = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn crate::tools::Tool>> = vec![Box::new(CountingTool::new(
        "browser__navigate",
        invocations,
    ))];
    let mcp_tool_names = mcp_set(&["browser__navigate"]);
    let groups = vec![ToolFilterGroup {
        mode: ToolFilterGroupMode::Dynamic,
        tools: vec!["browser__*".into()],
        keywords: vec!["browse".into()],
    }];
    let tool_descs: Vec<(&str, &str)> = Vec::new();
    let risk_profile = RiskProfileConfig::default();

    // Interactive startup with no message used to save this as
    // `base_system_prompt`, which advertises native tool availability.
    let startup_prompt = super::build_system_prompt_for_turn(
        workspace.path(),
        "test-model",
        &tool_descs,
        "",
        &[],
        None,
        None,
        &risk_profile,
        &provider,
        &tools_registry,
        &[],
        None,
        false,
        SkillsPromptInjectionMode::Full,
        false,
        usize::MAX,
        true,
        false,
        None,
        crate::agent::persona_projection::LegacyPersonaFiles::Inject,
        None,
    )
    .expect("startup prompt should build");
    assert!(startup_prompt.contains(NATIVE_TOOLS_TASK_FRAMING));
    assert!(!startup_prompt.contains(NO_TOOLS_TASK_FRAMING));

    let excluded_tools = super::compute_excluded_mcp_tools(
        &tools_registry,
        &groups,
        "read the local file",
        &mcp_tool_names,
    );
    assert_eq!(excluded_tools, vec!["browser__navigate".to_string()]);
    let no_tools_turn_prompt = super::build_system_prompt_for_turn(
        workspace.path(),
        "test-model",
        &tool_descs,
        "",
        &[],
        None,
        None,
        &risk_profile,
        &provider,
        &tools_registry,
        &excluded_tools,
        None,
        false,
        SkillsPromptInjectionMode::Full,
        false,
        usize::MAX,
        true,
        false,
        None,
        crate::agent::persona_projection::LegacyPersonaFiles::Inject,
        None,
    )
    .expect("no-tools turn prompt should build");
    assert!(
        no_tools_turn_prompt.contains(NO_TOOLS_TASK_FRAMING),
        "turn prompt must not inherit startup native-tool framing when dynamic filters exclude all MCP specs"
    );
    assert!(!no_tools_turn_prompt.contains(NATIVE_TOOLS_TASK_FRAMING));

    let included_tools = super::compute_excluded_mcp_tools(
        &tools_registry,
        &groups,
        "browse the site",
        &mcp_tool_names,
    );
    assert!(included_tools.is_empty());
    let tools_turn_prompt = super::build_system_prompt_for_turn(
        workspace.path(),
        "test-model",
        &tool_descs,
        "",
        &[],
        None,
        None,
        &risk_profile,
        &provider,
        &tools_registry,
        &included_tools,
        None,
        false,
        SkillsPromptInjectionMode::Full,
        false,
        usize::MAX,
        true,
        false,
        None,
        crate::agent::persona_projection::LegacyPersonaFiles::Inject,
        None,
    )
    .expect("tools turn prompt should build");
    assert!(tools_turn_prompt.contains(NATIVE_TOOLS_TASK_FRAMING));
    assert!(!tools_turn_prompt.contains(NO_TOOLS_TASK_FRAMING));
}

/// Regression guard for the `persona_section` parameter added to
/// `build_system_prompt_for_turn`: it must forward verbatim into the
/// built prompt when set, and add nothing when `None` (every caller of
/// this function before persona wiring).
#[test]
fn build_system_prompt_for_turn_forwards_persona_section() {
    use zeroclaw_config::schema::{RiskProfileConfig, SkillsPromptInjectionMode};

    let workspace = tempdir().unwrap();
    let provider = ScriptedModelProvider::from_text_responses(vec!["ok"]);
    let tools_registry: Vec<Box<dyn crate::tools::Tool>> = Vec::new();
    let tool_descs: Vec<(&str, &str)> = Vec::new();
    let risk_profile = RiskProfileConfig::default();

    let with_persona = super::build_system_prompt_for_turn(
        workspace.path(),
        "test-model",
        &tool_descs,
        "",
        &[],
        None,
        None,
        &risk_profile,
        &provider,
        &tools_registry,
        &[],
        None,
        false,
        SkillsPromptInjectionMode::Full,
        false,
        usize::MAX,
        true,
        false,
        Some("## Tone\n\n- Be terse.\n"),
        crate::agent::persona_projection::LegacyPersonaFiles::Inject,
        None,
    )
    .expect("prompt with persona section should build");
    assert!(
        with_persona.contains("## Tone"),
        "persona_section: Some(..) must reach the built prompt"
    );

    let without_persona = super::build_system_prompt_for_turn(
        workspace.path(),
        "test-model",
        &tool_descs,
        "",
        &[],
        None,
        None,
        &risk_profile,
        &provider,
        &tools_registry,
        &[],
        None,
        false,
        SkillsPromptInjectionMode::Full,
        false,
        usize::MAX,
        true,
        false,
        None,
        crate::agent::persona_projection::LegacyPersonaFiles::Inject,
        None,
    )
    .expect("prompt without persona section should build");
    assert!(
        !without_persona.contains("## Tone"),
        "persona_section: None must add no Tone section"
    );
}

#[test]
fn non_native_system_prompt_with_no_tools_contains_zero_tool_protocol() {
    use crate::agent::system_prompt::build_system_prompt_with_mode;

    let tool_summaries: Vec<(&str, &str)> = vec![];

    let system_prompt = build_system_prompt_with_mode(
        std::path::Path::new("/tmp"),
        "test-model",
        &tool_summaries,
        &[],
        None,
        None,
        false,
        zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
        crate::security::AutonomyLevel::default(),
    );

    assert!(
        !system_prompt.contains("## Tools"),
        "No-tools prompt must not include a Tools section"
    );
    assert!(
        !system_prompt.contains("## Tool Use Protocol"),
        "No-tools prompt must not include tool protocol"
    );
    assert!(
        !system_prompt.contains("<tool_call>"),
        "No-tools prompt must not mention XML tool calls"
    );
    assert!(
        !system_prompt.contains("<tool_result>"),
        "No-tools prompt must not mention XML tool results"
    );
    assert!(
        !system_prompt.contains("Use the tools"),
        "No-tools prompt must not instruct the model to use unavailable tools"
    );
    assert!(
        system_prompt.contains("No tools are available for this turn"),
        "No-tools prompt should explicitly describe the current capability boundary"
    );
}

#[test]
fn strict_non_native_prompt_policy_hides_text_tool_protocol_inputs() {
    let mut tool_descs = vec![("shell", "Run commands")];
    let mut deferred_section = "## Deferred MCP Tools\n\n- mcp__example".to_string();

    let expose_text_protocol =
        apply_text_tool_prompt_policy(false, true, &mut tool_descs, &mut deferred_section);

    assert!(!expose_text_protocol);
    assert!(
        tool_descs.is_empty(),
        "strict non-native prompt paths must not advertise text tools"
    );
    assert!(
        deferred_section.is_empty(),
        "strict non-native prompt paths must not advertise deferred text tools"
    );
}

// ── Cross-Alias & GLM Shortened Body Tests ──────────────────────────

// ═══════════════════════════════════════════════════════════════════════
// reasoning_content pass-through tests for history builders
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn build_native_assistant_history_includes_reasoning_content() {
    let calls = vec![ToolCall {
        id: "call_1".into(),
        name: "shell".into(),
        arguments: "{}".into(),
        extra_content: None,
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
        extra_content: None,
    }];
    let result = build_native_assistant_history("answer", &calls, None);
    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(parsed["content"].as_str(), Some("answer"));
    assert!(parsed.get("reasoning_content").is_none());
}

#[tokio::test]
async fn consume_provider_streaming_response_captures_reasoning_content() {
    let model_provider = StreamingNativeToolEventModelProvider::with_turns(vec![
        NativeStreamTurn::TextWithReasoning {
            text: "Listing the directory now.".to_string(),
            reasoning: "I need to call the shell tool to list files.".to_string(),
        },
    ]);
    let messages = vec![ChatMessage::user(
        "List the folders in the current directory",
    )];

    let outcome = consume_provider_streaming_response(
        &model_provider,
        &messages,
        None,
        &std::collections::HashSet::new(),
        "deepseek-v4-pro",
        Some(0.2),
        None,
        None,
        None, // event_tx
        false,
    )
    .await
    .expect("streaming should succeed");

    assert_eq!(outcome.response_text, "Listing the directory now.");
    assert_eq!(
        outcome.reasoning_content,
        "I need to call the shell tool to list files."
    );
    assert!(
        outcome.tool_calls.is_empty(),
        "this turn does not emit native tool calls"
    );
}

#[tokio::test]
async fn consume_provider_streaming_response_accumulates_split_reasoning_chunks() {
    // Scripted multi-event stream: two reasoning chunks straddling a text
    // delta. The outcome should concatenate the reasoning chunks in order
    // and keep them out of the visible response text.
    struct MultiChunkModelProvider;

    #[async_trait]
    impl ModelProvider for MultiChunkModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("not used in this test")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("not used in this test")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<StreamEvent>,
        > {
            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta(StreamChunk::reasoning("Step 1: "))),
                Ok(StreamEvent::TextDelta(StreamChunk::delta("Hello "))),
                Ok(StreamEvent::TextDelta(StreamChunk::reasoning(
                    "consider options.",
                ))),
                Ok(StreamEvent::TextDelta(StreamChunk::delta("there."))),
                Ok(StreamEvent::Final),
            ]))
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for MultiChunkModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "MultiChunkModelProvider"
        }
    }

    let model_provider = MultiChunkModelProvider;
    let messages = vec![ChatMessage::user("hi")];

    let outcome = consume_provider_streaming_response(
        &model_provider,
        &messages,
        None,
        &std::collections::HashSet::new(),
        "deepseek-v4-flash",
        Some(0.2),
        None,
        None,
        None, // event_tx
        false,
    )
    .await
    .expect("streaming should succeed");

    assert_eq!(outcome.response_text, "Hello there.");
    assert_eq!(outcome.reasoning_content, "Step 1: consider options.");
}

// ── glob_match tests ──────────────────────────────────────────────────────

#[test]
fn glob_match_exact_no_wildcard() {
    assert!(glob_match("mcp_browser_navigate", "mcp_browser_navigate"));
    assert!(!glob_match("mcp_browser_navigate", "mcp_browser_click"));
}

#[test]
fn glob_match_prefix_wildcard() {
    // Suffix pattern: mcp_browser_*
    assert!(glob_match("mcp_browser_*", "mcp_browser_navigate"));
    assert!(glob_match("mcp_browser_*", "mcp_browser_click"));
    assert!(!glob_match("mcp_browser_*", "mcp_filesystem_read"));

    // Prefix pattern: *_read
    assert!(glob_match("*_read", "mcp_filesystem_read"));
    assert!(!glob_match("*_read", "mcp_filesystem_write"));

    // Infix: mcp_*_navigate
    assert!(glob_match("mcp_*_navigate", "mcp_browser_navigate"));
    assert!(!glob_match("mcp_*_navigate", "mcp_browser_click"));
}

#[test]
fn glob_match_star_matches_everything() {
    assert!(glob_match("*", "anything_at_all"));
    assert!(glob_match("*", ""));
}

fn make_spec(name: &str) -> crate::tools::ToolSpec {
    crate::tools::ToolSpec::new(name.to_string(), String::new(), serde_json::json!({}))
}

fn mcp_set(names: &[&str]) -> HashSet<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

#[test]
fn filter_tool_specs_no_groups_returns_all() {
    let specs = vec![
        make_spec("shell_exec"),
        make_spec("browser__navigate"),
        make_spec("files__read_file"),
    ];
    let result = filter_tool_specs_for_turn(
        specs,
        &[],
        "hello",
        &mcp_set(&["browser__navigate", "files__read_file"]),
    );
    assert_eq!(result.len(), 3);
}

#[test]
fn filter_tool_specs_always_group_includes_matching_mcp_tool() {
    use zeroclaw_config::schema::{ToolFilterGroup, ToolFilterGroupMode};

    let specs = vec![
        make_spec("shell_exec"),
        make_spec("browser__navigate"),
        make_spec("files__read_file"),
    ];
    let groups = vec![ToolFilterGroup {
        mode: ToolFilterGroupMode::Always,
        tools: vec!["files__*".into()],
        keywords: vec![],
    }];
    let result = filter_tool_specs_for_turn(
        specs,
        &groups,
        "anything",
        &mcp_set(&["browser__navigate", "files__read_file"]),
    );
    let names: Vec<&str> = result.iter().map(|s| s.name.as_str()).collect();
    // Built-in passes through, matched MCP passes, unmatched MCP excluded.
    assert!(names.contains(&"shell_exec"));
    assert!(names.contains(&"files__read_file"));
    assert!(!names.contains(&"browser__navigate"));
}

#[test]
fn filter_tool_specs_group_matching_nothing_excludes_all_mcp_tools() {
    use zeroclaw_config::schema::{ToolFilterGroup, ToolFilterGroupMode};

    // The formerly-broken case the old prefix gate passed every
    // real `<server>__<tool>` name unfiltered. A group set matching none
    // of them must now hide ALL MCP-origin tools while built-ins survive.
    let specs = vec![
        make_spec("shell_exec"),
        make_spec("files__list"),
        make_spec("lights__get_state"),
    ];
    let groups = vec![ToolFilterGroup {
        mode: ToolFilterGroupMode::Always,
        tools: vec!["nonexistent__*".into()],
        keywords: vec![],
    }];
    let result = filter_tool_specs_for_turn(
        specs,
        &groups,
        "anything",
        &mcp_set(&["files__list", "lights__get_state"]),
    );
    let names: Vec<&str> = result.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["shell_exec"]);
}

#[test]
fn filter_tool_specs_skill_shaped_name_passes_through() {
    use zeroclaw_config::schema::{ToolFilterGroup, ToolFilterGroupMode};

    // Skill tools are named `{skill}__{tool}` (skill_tool.rs) — the same
    // shape as MCP tools. Classification is by origin (set membership),
    // never by name shape, so a skill tool must survive groups that would
    // exclude an identically-shaped MCP name.
    let specs = vec![make_spec("pdf_skill__extract"), make_spec("files__list")];
    let groups = vec![ToolFilterGroup {
        mode: ToolFilterGroupMode::Always,
        tools: vec!["nothing__*".into()],
        keywords: vec![],
    }];
    let result = filter_tool_specs_for_turn(specs, &groups, "anything", &mcp_set(&["files__list"]));
    let names: Vec<&str> = result.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"pdf_skill__extract"));
    assert!(!names.contains(&"files__list"));
}

#[test]
fn filter_tool_specs_capability_tools_remain_excludable() {
    use zeroclaw_config::schema::{ToolFilterGroup, ToolFilterGroupMode};

    let specs = vec![make_spec("mcp_resources"), make_spec("files__list")];
    let groups = vec![ToolFilterGroup {
        mode: ToolFilterGroupMode::Always,
        tools: vec!["files__*".into()],
        keywords: vec![],
    }];
    let result = filter_tool_specs_for_turn(
        specs,
        &groups,
        "anything",
        &mcp_set(&["mcp_resources", "files__list"]),
    );
    let names: Vec<&str> = result.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"files__list"));
    assert!(!names.contains(&"mcp_resources"));
}

#[test]
fn filter_tool_specs_dynamic_group_included_on_keyword_match() {
    use zeroclaw_config::schema::{ToolFilterGroup, ToolFilterGroupMode};

    let specs = vec![make_spec("shell_exec"), make_spec("browser__navigate")];
    let groups = vec![ToolFilterGroup {
        mode: ToolFilterGroupMode::Dynamic,
        tools: vec!["browser__*".into()],
        keywords: vec!["browse".into(), "website".into()],
    }];
    let result = filter_tool_specs_for_turn(
        specs,
        &groups,
        "please browse this page",
        &mcp_set(&["browser__navigate"]),
    );
    let names: Vec<&str> = result.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"shell_exec"));
    assert!(names.contains(&"browser__navigate"));
}

#[test]
fn filter_tool_specs_dynamic_group_excluded_on_no_keyword_match() {
    use zeroclaw_config::schema::{ToolFilterGroup, ToolFilterGroupMode};

    let specs = vec![make_spec("shell_exec"), make_spec("browser__navigate")];
    let groups = vec![ToolFilterGroup {
        mode: ToolFilterGroupMode::Dynamic,
        tools: vec!["browser__*".into()],
        keywords: vec!["browse".into(), "website".into()],
    }];
    let result = filter_tool_specs_for_turn(
        specs,
        &groups,
        "read the file /etc/hosts",
        &mcp_set(&["browser__navigate"]),
    );
    let names: Vec<&str> = result.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"shell_exec"));
    assert!(!names.contains(&"browser__navigate"));
}

#[test]
fn filter_tool_specs_dynamic_keyword_match_is_case_insensitive() {
    use zeroclaw_config::schema::{ToolFilterGroup, ToolFilterGroupMode};

    let specs = vec![make_spec("browser__navigate")];
    let groups = vec![ToolFilterGroup {
        mode: ToolFilterGroupMode::Dynamic,
        tools: vec!["browser__*".into()],
        keywords: vec!["Browse".into()],
    }];
    let result = filter_tool_specs_for_turn(
        specs,
        &groups,
        "BROWSE the site",
        &mcp_set(&["browser__navigate"]),
    );
    assert_eq!(result.len(), 1);
}

// ── compute_excluded_mcp_tools tests ───────────────────────────────────────

fn counting_registry(names: &[&str]) -> Vec<Box<dyn crate::tools::Tool>> {
    names
        .iter()
        .map(|name| {
            Box::new(CountingTool::new(name, Arc::new(AtomicUsize::new(0))))
                as Box<dyn crate::tools::Tool>
        })
        .collect()
}

#[test]
fn compute_excluded_mcp_tools_returns_real_prefixed_names() {
    use zeroclaw_config::schema::{ToolFilterGroup, ToolFilterGroupMode};

    let tools_registry = counting_registry(&["shell_exec", "files__list", "browser__navigate"]);
    let groups = vec![ToolFilterGroup {
        mode: ToolFilterGroupMode::Always,
        tools: vec!["files__*".into()],
        keywords: vec![],
    }];
    let excluded = super::compute_excluded_mcp_tools(
        &tools_registry,
        &groups,
        "anything",
        &mcp_set(&["files__list", "browser__navigate"]),
    );
    // Exactly the unmatched MCP-origin name — never the built-in.
    assert_eq!(excluded, vec!["browser__navigate".to_string()]);
}

#[test]
fn compute_excluded_mcp_tools_skill_tool_never_excluded() {
    use zeroclaw_config::schema::{ToolFilterGroup, ToolFilterGroupMode};

    let tools_registry = counting_registry(&["pdf_skill__extract", "files__list"]);
    let groups = vec![ToolFilterGroup {
        mode: ToolFilterGroupMode::Always,
        tools: vec!["nothing__*".into()],
        keywords: vec![],
    }];
    let excluded = super::compute_excluded_mcp_tools(
        &tools_registry,
        &groups,
        "anything",
        &mcp_set(&["files__list"]),
    );
    assert!(!excluded.contains(&"pdf_skill__extract".to_string()));
    assert_eq!(excluded, vec!["files__list".to_string()]);
}

#[test]
fn compute_excluded_mcp_tools_empty_origin_set_is_noop() {
    use zeroclaw_config::schema::{ToolFilterGroup, ToolFilterGroupMode};

    // MCP disabled/unconfigured/failed: the origin set is empty, nothing
    // is classified MCP, and groups are inert by construction.
    let tools_registry = counting_registry(&["shell_exec", "files__list"]);
    let groups = vec![ToolFilterGroup {
        mode: ToolFilterGroupMode::Always,
        tools: vec!["files__*".into()],
        keywords: vec![],
    }];
    let excluded =
        super::compute_excluded_mcp_tools(&tools_registry, &groups, "anything", &HashSet::new());
    assert!(excluded.is_empty());
}

#[test]
fn compute_excluded_mcp_tools_deferred_stub_registry_baseline() {
    use zeroclaw_config::schema::{ToolFilterGroup, ToolFilterGroupMode};

    let tools_registry = counting_registry(&["shell_exec", "tool_search"]);
    let groups = vec![ToolFilterGroup {
        mode: ToolFilterGroupMode::Always,
        tools: vec!["nothing__*".into()],
        keywords: vec![],
    }];
    let excluded = super::compute_excluded_mcp_tools(
        &tools_registry,
        &groups,
        "anything",
        &mcp_set(&["files__list", "lights__get_state"]),
    );
    assert!(excluded.is_empty());
}

// ── preactivate_always_filter_groups tests ─────────────────────────────────

async fn make_deferred_set(names: &[&str]) -> crate::tools::DeferredMcpToolSet {
    let registry = Arc::new(
        crate::tools::McpRegistry::connect_all(&[])
            .await
            .expect("empty MCP registry connects"),
    );
    let stubs = names
        .iter()
        .map(|name| {
            zeroclaw_tools::mcp_deferred::DeferredMcpToolStub::new(
                (*name).to_string(),
                zeroclaw_tools::mcp_protocol::McpToolDef {
                    name: (*name).to_string(),
                    description: Some("test tool".to_string()),
                    input_schema: serde_json::json!({"type": "object", "properties": {}}),
                },
            )
        })
        .collect();
    crate::tools::DeferredMcpToolSet { stubs, registry }
}

fn always_group(patterns: &[&str]) -> Vec<zeroclaw_config::schema::ToolFilterGroup> {
    use zeroclaw_config::schema::{ToolFilterGroup, ToolFilterGroupMode};
    vec![ToolFilterGroup {
        mode: ToolFilterGroupMode::Always,
        tools: patterns.iter().map(|p| (*p).to_string()).collect(),
        keywords: vec![],
    }]
}

#[tokio::test]
async fn preactivate_always_group_activates_matched_stubs() {
    let deferred = make_deferred_set(&["files__list", "lights__get_state"]).await;
    let activated = Arc::new(Mutex::new(crate::tools::ActivatedToolSet::new()));

    let names = super::preactivate_always_filter_groups(
        &deferred,
        &activated,
        &always_group(&["files__*"]),
        None,
    );

    assert_eq!(names, mcp_set(&["files__list"]));
    let guard = activated.lock().unwrap();
    assert!(guard.is_activated("files__list"));
    assert!(!guard.is_activated("lights__get_state"));
}

#[tokio::test]
async fn preactivate_skips_dynamic_groups() {
    use zeroclaw_config::schema::{ToolFilterGroup, ToolFilterGroupMode};

    let deferred = make_deferred_set(&["files__list"]).await;
    let activated = Arc::new(Mutex::new(crate::tools::ActivatedToolSet::new()));
    let groups = vec![ToolFilterGroup {
        mode: ToolFilterGroupMode::Dynamic,
        tools: vec!["files__*".into()],
        keywords: vec!["file".into()],
    }];

    let names = super::preactivate_always_filter_groups(&deferred, &activated, &groups, None);

    assert!(names.is_empty());
    assert!(!activated.lock().unwrap().is_activated("files__list"));
}

#[tokio::test]
async fn preactivate_is_idempotent_on_repeat_call() {
    let deferred = make_deferred_set(&["files__list"]).await;
    let activated = Arc::new(Mutex::new(crate::tools::ActivatedToolSet::new()));
    let groups = always_group(&["files__*"]);

    let first = super::preactivate_always_filter_groups(&deferred, &activated, &groups, None);
    let second = super::preactivate_always_filter_groups(&deferred, &activated, &groups, None);

    assert_eq!(first, mcp_set(&["files__list"]));
    assert!(second.is_empty());
    assert!(activated.lock().unwrap().is_activated("files__list"));
}

#[tokio::test]
async fn preactivate_respects_mcp_access_policy() {
    let deferred = make_deferred_set(&["files__list", "files__write"]).await;
    let activated = Arc::new(Mutex::new(crate::tools::ActivatedToolSet::new()));
    // The risk-profile denylist always wins over `mode = "always"`.
    let excluded = vec!["files__write".to_string()];
    let policy = zeroclaw_tools::tool_search::ToolAccessPolicy::from_security(
        None,
        Some(&excluded),
        None,
        zeroclaw_config::autonomy::McpDiscoveredToolPolicy::default(),
    )
    .expect("denylist yields a policy");

    let names = super::preactivate_always_filter_groups(
        &deferred,
        &activated,
        &always_group(&["files__*"]),
        Some(&policy),
    );

    assert_eq!(names, mcp_set(&["files__list"]));
    let guard = activated.lock().unwrap();
    assert!(guard.is_activated("files__list"));
    assert!(!guard.is_activated("files__write"));
}

// ── Token-based compaction tests ──────────────────────────

#[test]
fn estimate_history_tokens_empty() {
    assert_eq!(super::estimate_history_tokens(&[]), 0);
}

#[test]
fn estimate_history_tokens_single_message() {
    let history = vec![ChatMessage::user("hello world")]; // 11 chars
    let tokens = super::estimate_history_tokens(&history);
    // 11.div_ceil(4) + 4 = 3 + 4 = 7
    assert_eq!(tokens, 7);
}

#[test]
fn estimate_history_tokens_multiple_messages() {
    let history = vec![
        ChatMessage::system("You are helpful."), // 16 chars → 4 + 4 = 8
        ChatMessage::user("What is Rust?"),      // 13 chars → 4 + 4 = 8
        ChatMessage::assistant("A language."),   // 11 chars → 3 + 4 = 7
    ];
    let tokens = super::estimate_history_tokens(&history);
    assert_eq!(tokens, 23);
}

#[test]
fn cli_outer_recovery_trims_below_model_window_with_headroom() {
    use crate::agent::history_trim::trim_to_recent_turns;

    let model_context_window: usize = 32_000;
    let recovery_budget = model_context_window * 9 / 10; // 28_800

    let big = "x".repeat(4000);
    let mut history = vec![ChatMessage::system("sys")];
    for i in 0..32 {
        history.push(ChatMessage::user(format!("turn {i} {big}").as_str()));
        history.push(ChatMessage::assistant(format!("reply {i} {big}").as_str()));
    }
    let tokens_before = super::estimate_history_tokens(&history);
    assert!(
        tokens_before > model_context_window,
        "fixture must overflow the window: got {tokens_before}"
    );

    let result = trim_to_recent_turns(history, recovery_budget);
    assert!(
        result.trimmed,
        "recovery must trim when the estimate exceeds the 90% budget"
    );
    assert!(
        result.tokens_after <= recovery_budget,
        "tokens_after ({}) must be <= recovery_budget ({})",
        result.tokens_after,
        recovery_budget
    );
    // Headroom must leave us strictly below the model's true window,
    // so the retried request has room for the reply + next user turn.
    assert!(
        result.tokens_after < model_context_window,
        "headroom must leave us strictly below the model window: got {}",
        result.tokens_after
    );
}

#[tokio::test]
async fn run_tool_call_loop_surfaces_tool_failure_reason_in_on_delta() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider = ScriptedModelProvider::from_text_responses(vec![
        r#"<tool_call>
{"name":"failing_shell","arguments":{"command":"rm -rf /"}}
</tool_call>"#,
        "I could not execute that command.",
    ]);

    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(FailingTool::new(
        "failing_shell",
        "Command not allowed by security policy: rm -rf /",
    ))];

    let mut history = vec![
        ChatMessage::system("test-system"),
        ChatMessage::user("delete everything"),
    ];
    let observer = NoopObserver;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(64);

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 4,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "telegram",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: Some(tx),
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("tool loop should complete");

    // Collect all messages sent to the on_delta channel.
    let mut deltas = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        deltas.push(msg);
    }

    let all_deltas: String = deltas
        .iter()
        .map(|d| match d {
            StreamDelta::Status(t) | StreamDelta::Text(t) => t.as_str(),
        })
        .collect();

    // The failure reason should appear in the progress messages.
    assert!(
        all_deltas.contains("Command not allowed by security policy"),
        "on_delta messages should include the tool failure reason, got: {all_deltas}"
    );

    // Should also contain the cross mark (❌) icon to indicate failure.
    assert!(
        all_deltas.contains('\u{274c}'),
        "on_delta messages should include ❌ for failed tool calls, got: {all_deltas}"
    );

    assert!(
        result.ends_with("I could not execute that command."),
        "result should end with error message, got: {result}"
    );
}

// ── filter_by_allowed_tools tests ─────────────────────────────────────

#[test]
fn filter_by_allowed_tools_none_passes_all() {
    let specs = vec![
        make_spec("shell"),
        make_spec("memory_store"),
        make_spec("file_read"),
    ];
    let result = filter_by_allowed_tools(specs, None);
    assert_eq!(result.len(), 3);
}

#[test]
fn filter_by_allowed_tools_some_restricts_to_listed() {
    let specs = vec![
        make_spec("shell"),
        make_spec("memory_store"),
        make_spec("file_read"),
    ];
    let allowed = vec!["shell".to_string(), "memory_store".to_string()];
    let result = filter_by_allowed_tools(specs, Some(&allowed));
    let names: Vec<&str> = result.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"shell"));
    assert!(names.contains(&"memory_store"));
    assert!(!names.contains(&"file_read"));
}

#[test]
fn filter_by_allowed_tools_unknown_names_silently_ignored() {
    let specs = vec![make_spec("shell"), make_spec("file_read")];
    let allowed = vec![
        "shell".to_string(),
        "nonexistent_tool".to_string(),
        "another_missing".to_string(),
    ];
    let result = filter_by_allowed_tools(specs, Some(&allowed));
    let names: Vec<&str> = result.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names.len(), 1);
    assert!(names.contains(&"shell"));
}

#[test]
fn filter_by_allowed_tools_empty_list_excludes_all() {
    let specs = vec![make_spec("shell"), make_spec("file_read")];
    let allowed: Vec<String> = vec![];
    let result = filter_by_allowed_tools(specs, Some(&allowed));
    assert!(result.is_empty());
}

// ── Cost tracking tests ──

#[tokio::test]
async fn cost_tracking_records_usage_when_scoped() {
    use super::{
        TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoop, ToolLoopCostTrackingContext, run_tool_call_loop,
    };
    use crate::cost::CostTracker;
    use crate::observability::noop::NoopObserver;
    use std::collections::HashMap;

    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider = ScriptedModelProvider {
        responses: Arc::new(Mutex::new(VecDeque::from([ChatResponse {
            text: Some("done".to_string()),
            tool_calls: Vec::new(),
            usage: Some(zeroclaw_providers::traits::TokenUsage {
                input_tokens: Some(1_000),
                output_tokens: Some(200),
                cached_input_tokens: None,
            }),
            reasoning_content: None,
        }]))),
        capabilities: ProviderCapabilities::default(),
    };
    let observer = NoopObserver;
    let workspace = tempfile::TempDir::new().unwrap();
    let cost_config = zeroclaw_config::schema::CostConfig {
        enabled: true,
        ..zeroclaw_config::schema::CostConfig::default()
    };
    let tracker = Arc::new(CostTracker::new(cost_config.clone(), workspace.path()).unwrap());
    let mut model_pricing: HashMap<String, f64> = HashMap::new();
    model_pricing.insert("mock-model.input".to_string(), 3.0);
    model_pricing.insert("mock-model.output".to_string(), 15.0);
    let mut pricing: crate::agent::cost::ModelProviderPricing = HashMap::new();
    pricing.insert("mock-provider".to_string(), model_pricing);
    let ctx = ToolLoopCostTrackingContext::new(Arc::clone(&tracker), Arc::new(pricing));
    let mut history = vec![ChatMessage::system("test"), ChatMessage::user("hello")];

    let result = TOOL_LOOP_COST_TRACKING_CONTEXT
        .scope(
            Some(ctx),
            run_tool_call_loop(ToolLoop {
                parent_agent_alias: None,
                exec: ResolvedAgentExecution {
                    task_owner: None,
                    model_access: ResolvedModelAccess {
                        model_provider: &model_provider,
                        provider_name: "mock-provider",
                        model: "mock-model",
                        temperature: Some(0.0),
                    },
                    tools_registry: &[],
                    observer: &observer,
                    silent: true,
                    approval: None,
                    multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                    config: None,
                    max_tool_iterations: 2,
                    hooks: None,
                    excluded_tools: &[],
                    dedup_exempt_tools: &[],
                    activated_tools: None,
                    model_switch_callback: None,
                    pacing: &zeroclaw_config::schema::PacingConfig::default(),
                    strict_tool_parsing: false,
                    parallel_tools: false,
                    max_tool_result_chars: 0,
                    keep_tool_context_turns: 2,
                    context_token_budget: 0,
                    receipt_generator: None,
                    knobs: &LoopKnobs::default(),
                },
                history: &mut history,
                channel_name: "test",
                channel_reply_target: None,
                cancellation_token: None,
                on_delta: None,
                shared_budget: None,
                channel: None,
                collected_receipts: None,
                event_tx: None,
                steering: None,
                new_messages_out: None,
                image_cache: None,
                // Phase 1: stamp Internal/Trusted. Per-transport
                // stamping lands in a later phase.
                memory: None,
                ingress: IngressContext::sub_turn(),
                agent_alias: None,
                turn_id: &turn_id,
            }),
        )
        .await
        .expect("tool loop should succeed");

    assert!(
        result.ends_with("done"),
        "result should end with 'done', got: {result}"
    );
    let summary = tracker.get_summary().unwrap();
    assert_eq!(summary.request_count, 1);
    assert_eq!(summary.total_tokens, 1_200);
    assert!(summary.session_cost_usd > 0.0);
}

#[tokio::test]
async fn tool_loop_normalizes_non_leading_system_messages_before_provider_request() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let provider = RecordingModelProvider::new();
    let requests = Arc::clone(&provider.requests);
    let observer = NoopObserver;
    let mut history = vec![
        ChatMessage::system("base system"),
        ChatMessage::user("first question"),
        ChatMessage::assistant("first answer"),
        ChatMessage::system("late loop-detection guidance"),
        ChatMessage::user("follow-up"),
    ];

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "recording-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &[],
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 2,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "test",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("tool loop should complete");

    assert_eq!(result, "done");
    let requests = requests.lock().expect("requests lock should be valid");
    assert_eq!(requests.len(), 1);
    let sent = &requests[0];
    assert_eq!(sent[0].role, "system");
    assert_eq!(
        sent.iter().filter(|msg| msg.role == "system").count(),
        1,
        "provider request must not contain non-leading system messages: {:?}",
        sent.iter().map(|msg| msg.role.as_str()).collect::<Vec<_>>()
    );
    assert!(sent[0].content.contains("base system"));
    assert!(sent[0].content.contains("late loop-detection guidance"));
    assert_eq!(
        sent.iter().map(|msg| msg.role.as_str()).collect::<Vec<_>>(),
        vec!["system", "user", "assistant", "user"]
    );
}

#[tokio::test]
async fn cost_tracking_enforces_budget() {
    use super::{
        TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoop, ToolLoopCostTrackingContext, run_tool_call_loop,
    };
    use crate::cost::CostTracker;
    use crate::observability::noop::NoopObserver;
    use std::collections::HashMap;

    let turn_id = uuid::Uuid::new_v4().to_string();
    let model_provider = ScriptedModelProvider::from_text_responses(vec!["should not reach this"]);
    let observer = NoopObserver;
    let workspace = tempfile::TempDir::new().unwrap();
    let cost_config = zeroclaw_config::schema::CostConfig {
        enabled: true,
        daily_limit_usd: 0.001, // very low limit
        ..zeroclaw_config::schema::CostConfig::default()
    };
    let tracker = Arc::new(CostTracker::new(cost_config.clone(), workspace.path()).unwrap());
    // Record a usage that already exceeds the limit
    tracker
        .record_usage(crate::cost::calculate_token_usage(
            "mock-model",
            100_000,
            50_000,
            0,
            1.0,
            1.0,
            0.0,
        ))
        .unwrap();

    let mut model_pricing: HashMap<String, f64> = HashMap::new();
    model_pricing.insert("mock-model.input".to_string(), 1.0);
    model_pricing.insert("mock-model.output".to_string(), 1.0);
    let mut pricing: crate::agent::cost::ModelProviderPricing = HashMap::new();
    pricing.insert("mock-provider".to_string(), model_pricing);
    let ctx = ToolLoopCostTrackingContext::new(Arc::clone(&tracker), Arc::new(pricing));
    let mut history = vec![ChatMessage::system("test"), ChatMessage::user("hello")];

    let err = TOOL_LOOP_COST_TRACKING_CONTEXT
        .scope(
            Some(ctx),
            run_tool_call_loop(ToolLoop {
                parent_agent_alias: None,
                exec: ResolvedAgentExecution {
                    task_owner: None,
                    model_access: ResolvedModelAccess {
                        model_provider: &model_provider,
                        provider_name: "mock-provider",
                        model: "mock-model",
                        temperature: Some(0.0),
                    },
                    tools_registry: &[],
                    observer: &observer,
                    silent: true,
                    approval: None,
                    multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                    config: None,
                    max_tool_iterations: 2,
                    hooks: None,
                    excluded_tools: &[],
                    dedup_exempt_tools: &[],
                    activated_tools: None,
                    model_switch_callback: None,
                    pacing: &zeroclaw_config::schema::PacingConfig::default(),
                    strict_tool_parsing: false,
                    parallel_tools: false,
                    max_tool_result_chars: 0,
                    keep_tool_context_turns: 2,
                    context_token_budget: 0,
                    receipt_generator: None,
                    knobs: &LoopKnobs::default(),
                },
                history: &mut history,
                channel_name: "test",
                channel_reply_target: None,
                cancellation_token: None,
                on_delta: None,
                shared_budget: None,
                channel: None,
                collected_receipts: None,
                event_tx: None,
                steering: None,
                new_messages_out: None,
                image_cache: None,
                // Phase 1: stamp Internal/Trusted. Per-transport
                // stamping lands in a later phase.
                memory: None,
                ingress: IngressContext::sub_turn(),
                agent_alias: None,
                turn_id: &turn_id,
            }),
        )
        .await
        .expect_err("should fail with budget exceeded");

    assert!(
        err.to_string().contains("Budget exceeded"),
        "error should mention budget: {err}"
    );
}

#[tokio::test]
async fn cost_tracking_is_noop_without_scope() {
    let turn_id = uuid::Uuid::new_v4().to_string();
    use super::{ToolLoop, run_tool_call_loop};
    use crate::observability::noop::NoopObserver;

    // No TOOL_LOOP_COST_TRACKING_CONTEXT scoped — should run fine
    let model_provider = ScriptedModelProvider {
        responses: Arc::new(Mutex::new(VecDeque::from([ChatResponse {
            text: Some("ok".to_string()),
            tool_calls: Vec::new(),
            usage: Some(zeroclaw_providers::traits::TokenUsage {
                input_tokens: Some(500),
                output_tokens: Some(100),
                cached_input_tokens: None,
            }),
            reasoning_content: None,
        }]))),
        capabilities: ProviderCapabilities::default(),
    };
    let observer = NoopObserver;
    let mut history = vec![ChatMessage::system("test"), ChatMessage::user("hello")];

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &[],
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 2,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "test",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted until per-transport
        // stamping lands.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("should succeed without cost scope");

    assert_eq!(result, "ok");
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn trim_record_carries_model_attribution() {
    use super::{ToolLoop, run_tool_call_loop};
    use crate::observability::noop::NoopObserver;
    use ::zeroclaw_log::Instrument;

    let _writer_guard = zeroclaw_log::__private_test_writer_lock();
    let _hook_guard = zeroclaw_log::__private_test_hook_lock();
    zeroclaw_log::try_install_capture_subscriber();
    let mut rx = zeroclaw_log::subscribe_or_install();
    while rx.try_recv().is_ok() {}

    let model_provider = ScriptedModelProvider {
        responses: Arc::new(Mutex::new(VecDeque::from([ChatResponse {
            text: Some("ok".to_string()),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        }]))),
        capabilities: ProviderCapabilities::default(),
    };
    let observer = NoopObserver;

    let big = "x".repeat(4000);
    let mut history = vec![
        ChatMessage::system("system"),
        ChatMessage::user(format!("turn1 {big}")),
        ChatMessage::assistant("a1"),
        ChatMessage::user(format!("turn2 {big}")),
        ChatMessage::assistant("a2"),
        ChatMessage::user("turn3 short"),
    ];

    let _ = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "anthropic.personal",
                model: "claude-opus-4-8",
                temperature: Some(0.0),
            },
            tools_registry: &[],
            observer: &observer,
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 1,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 50,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "test",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: "test-turn-id",
    })
    .instrument(zeroclaw_log::attribution_span!(
        &crate::agent::AgentAttribution("trimtest")
    ))
    .await
    .expect("loop should succeed");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut trim_event = None;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
            Ok(Ok(value)) => {
                if value
                    .get("message")
                    .and_then(|v| v.as_str())
                    .is_some_and(|m| m.starts_with("History trimmed:"))
                {
                    trim_event = Some(value);
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => continue,
        }
    }

    let value = trim_event.expect("trim record must be emitted when history exceeds budget");
    assert_eq!(
        value["zeroclaw"]["model"], "claude-opus-4-8",
        "trim record must carry model attribution, got: {value}"
    );
    assert_eq!(
        value["zeroclaw"]["model_provider"], "anthropic.personal",
        "trim record must carry model_provider attribution, got: {value}"
    );
    assert_eq!(
        value["zeroclaw"]["agent_alias"], "trimtest",
        "trim record must inherit agent_alias from the agent attribution span, got: {value}"
    );
}

fn review_test_pricing() -> crate::agent::cost::ModelProviderPricing {
    use std::collections::HashMap;
    let mut model_pricing: HashMap<String, f64> = HashMap::new();
    model_pricing.insert("mock-model.input".to_string(), 3.0);
    model_pricing.insert("mock-model.output".to_string(), 15.0);
    let mut pricing: crate::agent::cost::ModelProviderPricing = HashMap::new();
    pricing.insert("mock-provider".to_string(), model_pricing);
    pricing
}

fn review_test_history() -> Vec<ChatMessage> {
    // One completed tool call so `should_trigger` (threshold 1) fires.
    vec![
        ChatMessage::system("test"),
        ChatMessage::user("hello"),
        ChatMessage::assistant("..."),
        ChatMessage {
            role: "tool".to_string(),
            content: "ok".to_string(),
        },
    ]
}

#[tokio::test]
async fn skill_review_fork_records_cost_usage_under_parent_scope() {
    use super::{TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoopCostTrackingContext};
    use crate::cost::CostTracker;
    use crate::observability::noop::NoopObserver;

    // Fork's single provider turn returns "Nothing to save." with usage —
    // no tool calls, so the fork ends after one recorded provider call.
    let model_provider = ScriptedModelProvider {
        responses: Arc::new(Mutex::new(VecDeque::from([ChatResponse {
            text: Some("Nothing to save.".to_string()),
            tool_calls: Vec::new(),
            usage: Some(zeroclaw_providers::traits::TokenUsage {
                input_tokens: Some(800),
                output_tokens: Some(120),
                cached_input_tokens: None,
            }),
            reasoning_content: None,
        }]))),
        capabilities: ProviderCapabilities::default(),
    };
    let observer = NoopObserver;
    let workspace = tempfile::TempDir::new().unwrap();
    let cost_config = zeroclaw_config::schema::CostConfig {
        enabled: true,
        ..zeroclaw_config::schema::CostConfig::default()
    };
    let tracker = Arc::new(CostTracker::new(cost_config, workspace.path()).unwrap());
    let ctx =
        ToolLoopCostTrackingContext::new(Arc::clone(&tracker), Arc::new(review_test_pricing()));

    let review_config = zeroclaw_config::schema::SkillImprovementConfig {
        enabled: true,
        cooldown_secs: 0,
        nudge_interval_iterations: 1,
        max_review_iterations: 2,
    };

    TOOL_LOOP_COST_TRACKING_CONTEXT
        .scope(
            Some(ctx),
            crate::skills::review::maybe_run_skill_review(
                None,
                workspace.path().to_path_buf(),
                review_config,
                false,
                review_test_history(),
                Vec::new(),
                &model_provider,
                "mock-provider",
                "mock-model",
                &observer,
                &zeroclaw_config::schema::MultimodalConfig::default(),
                &zeroclaw_config::schema::PacingConfig::default(),
                0,
                0,
                None,
                None, // agent_alias — no parent alias in the review-fork test fixture
            ),
        )
        .await;

    let summary = tracker.get_summary().unwrap();
    assert_eq!(
        summary.request_count, 1,
        "review fork must record its provider call under the parent cost scope"
    );
    assert_eq!(summary.total_tokens, 920);
    assert!(summary.session_cost_usd > 0.0);
}

#[tokio::test]
async fn skill_review_fork_respects_budget_under_parent_scope() {
    use super::{TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoopCostTrackingContext};
    use crate::cost::CostTracker;
    use crate::observability::noop::NoopObserver;

    // Provider that WOULD record usage if reached — but the pre-exceeded
    // budget must block the call before it happens.
    let model_provider = ScriptedModelProvider {
        responses: Arc::new(Mutex::new(VecDeque::from([ChatResponse {
            text: Some("should not be reached".to_string()),
            tool_calls: Vec::new(),
            usage: Some(zeroclaw_providers::traits::TokenUsage {
                input_tokens: Some(800),
                output_tokens: Some(120),
                cached_input_tokens: None,
            }),
            reasoning_content: None,
        }]))),
        capabilities: ProviderCapabilities::default(),
    };
    let observer = NoopObserver;
    let workspace = tempfile::TempDir::new().unwrap();
    let cost_config = zeroclaw_config::schema::CostConfig {
        enabled: true,
        daily_limit_usd: 0.001, // very low limit
        ..zeroclaw_config::schema::CostConfig::default()
    };
    let tracker = Arc::new(CostTracker::new(cost_config, workspace.path()).unwrap());
    // Record usage that already exceeds the daily limit.
    tracker
        .record_usage(crate::cost::calculate_token_usage(
            "mock-model",
            100_000,
            50_000,
            0,
            1.0,
            1.0,
            0.0,
        ))
        .unwrap();
    let before = tracker.get_summary().unwrap().request_count;

    let ctx =
        ToolLoopCostTrackingContext::new(Arc::clone(&tracker), Arc::new(review_test_pricing()));
    let review_config = zeroclaw_config::schema::SkillImprovementConfig {
        enabled: true,
        cooldown_secs: 0,
        nudge_interval_iterations: 1,
        max_review_iterations: 2,
    };

    TOOL_LOOP_COST_TRACKING_CONTEXT
        .scope(
            Some(ctx),
            crate::skills::review::maybe_run_skill_review(
                None,
                workspace.path().to_path_buf(),
                review_config,
                false,
                review_test_history(),
                Vec::new(),
                &model_provider,
                "mock-provider",
                "mock-model",
                &observer,
                &zeroclaw_config::schema::MultimodalConfig::default(),
                &zeroclaw_config::schema::PacingConfig::default(),
                0,
                0,
                None,
                None, // agent_alias — no parent alias in the review-fork test fixture
            ),
        )
        .await;

    let after = tracker.get_summary().unwrap().request_count;
    assert_eq!(
        after, before,
        "budget-exceeded fork must not make a recorded provider call"
    );
}

#[tokio::test]
async fn reflected_skill_creation_records_cost_usage_under_parent_scope() {
    use super::{TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoopCostTrackingContext};
    use crate::cost::CostTracker;
    use crate::skills::creator::{SkillCreator, ToolCallRecord};

    // Reflection's single post-turn provider call returns a valid SKILL.md
    // with usage. It must go through `chat` (the scripted provider bails on
    // `chat_with_system`), and record its usage under the parent scope.
    let model_provider = ScriptedModelProvider {
        responses: Arc::new(Mutex::new(VecDeque::from([ChatResponse {
            text: Some(
                "---\nname: model-picked\ndescription: Build and test the project.\n---\n\n# X\n\nBody.\n"
                    .to_string(),
            ),
            tool_calls: Vec::new(),
            usage: Some(zeroclaw_providers::traits::TokenUsage {
                input_tokens: Some(800),
                output_tokens: Some(120),
                cached_input_tokens: None,
            }),
            reasoning_content: None,
        }]))),
        capabilities: ProviderCapabilities::default(),
    };

    let dir = tempfile::TempDir::new().unwrap();
    let creator = SkillCreator::new(
        dir.path().to_path_buf(),
        zeroclaw_config::schema::SkillCreationConfig {
            enabled: true,
            reflection_enabled: true,
            ..zeroclaw_config::schema::SkillCreationConfig::default()
        },
    );
    let tool_calls = vec![
        ToolCallRecord {
            name: "shell".to_string(),
            args: serde_json::json!({ "cmd": "build" }),
        },
        ToolCallRecord {
            name: "shell".to_string(),
            args: serde_json::json!({ "cmd": "test" }),
        },
    ];

    let workspace = tempfile::TempDir::new().unwrap();
    let cost_config = zeroclaw_config::schema::CostConfig {
        enabled: true,
        ..zeroclaw_config::schema::CostConfig::default()
    };
    let tracker = Arc::new(CostTracker::new(cost_config, workspace.path()).unwrap());
    let ctx =
        ToolLoopCostTrackingContext::new(Arc::clone(&tracker), Arc::new(review_test_pricing()));

    let slug = TOOL_LOOP_COST_TRACKING_CONTEXT
        .scope(
            Some(ctx),
            creator.create_from_execution_reflected(
                "Build and test the project",
                &tool_calls,
                "All tests passed.",
                None,
                "mock-provider",
                &model_provider,
                "mock-model",
            ),
        )
        .await
        .unwrap()
        .expect("a skill should be created from the reflected SKILL.md");
    assert_eq!(slug, "build-and-test-the-project");

    let summary = tracker.get_summary().unwrap();
    assert_eq!(
        summary.request_count, 1,
        "reflection must record its provider call under the parent cost scope"
    );
    assert_eq!(summary.total_tokens, 920);
    assert!(summary.session_cost_usd > 0.0);
}

use zeroclaw_api::tool::Tool as TestTool;
use zeroclaw_policy::SecurityPolicy as TestPolicy;

struct NamedMockTool {
    the_name: &'static str,
}

#[async_trait]
impl TestTool for NamedMockTool {
    fn name(&self) -> &str {
        self.the_name
    }
    fn description(&self) -> &str {
        ""
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<crate::tools::ToolResult> {
        Ok(crate::tools::ToolResult {
            success: true,
            output: crate::tools::ToolOutput::default(),
            error: None,
        })
    }
}

fn mock_tool(name: &'static str) -> Box<dyn TestTool> {
    Box::new(NamedMockTool { the_name: name })
}

fn mock_tool_arc(name: &'static str) -> std::sync::Arc<dyn TestTool> {
    std::sync::Arc::new(NamedMockTool { the_name: name })
}

fn tool_names(tools: &[Box<dyn TestTool>]) -> Vec<&str> {
    tools.iter().map(|t| t.name()).collect()
}

#[test]
fn apply_policy_tool_filter_no_gates_keeps_everything() {
    let mut tools = vec![
        mock_tool("shell"),
        mock_tool("reasoning_subagent"),
        mock_tool("memory_recall"),
    ];
    super::apply_policy_tool_filter(&mut tools, None, None);
    assert_eq!(
        tool_names(&tools),
        vec!["shell", "reasoning_subagent", "memory_recall"]
    );
}

#[test]
fn apply_policy_tool_filter_policy_allowlist_restricts() {
    let mut tools = vec![
        mock_tool("shell"),
        mock_tool("reasoning_subagent"),
        mock_tool("memory_recall"),
    ];
    let policy = TestPolicy {
        allowed_tools: Some(vec!["shell".into(), "memory_recall".into()]),
        ..TestPolicy::default()
    };

    super::apply_policy_tool_filter(&mut tools, Some(&policy), None);
    assert_eq!(tool_names(&tools), vec!["shell", "memory_recall"]);
}

#[test]
fn apply_policy_tool_filter_policy_excluded_subtracts_from_unrestricted() {
    let mut tools = vec![mock_tool("shell"), mock_tool("reasoning_subagent")];
    let policy = TestPolicy {
        excluded_tools: Some(vec!["reasoning_subagent".into()]),
        ..TestPolicy::default()
    };

    super::apply_policy_tool_filter(&mut tools, Some(&policy), None);
    assert_eq!(tool_names(&tools), vec!["shell"]);
}

#[test]
fn apply_policy_tool_filter_caller_filter_alone_restricts() {
    let mut tools = vec![
        mock_tool("shell"),
        mock_tool("reasoning_subagent"),
        mock_tool("memory_recall"),
    ];
    let caller = vec!["memory_recall".to_string()];

    super::apply_policy_tool_filter(&mut tools, None, Some(&caller));
    assert_eq!(tool_names(&tools), vec!["memory_recall"]);
}

#[test]
fn apply_policy_tool_filter_policy_and_caller_intersect() {
    let mut tools = vec![
        mock_tool("shell"),
        mock_tool("reasoning_subagent"),
        mock_tool("memory_recall"),
    ];
    let policy = TestPolicy {
        allowed_tools: Some(vec!["shell".into(), "memory_recall".into()]),
        ..TestPolicy::default()
    };
    let caller = vec!["shell".to_string(), "reasoning_subagent".to_string()];

    super::apply_policy_tool_filter(&mut tools, Some(&policy), Some(&caller));
    // Only `shell` survives — it's the intersection of the policy
    // allowlist {shell, memory_recall} and the caller filter
    // {shell, reasoning_subagent}.
    assert_eq!(tool_names(&tools), vec!["shell"]);
}

#[test]
fn apply_policy_tool_filter_policy_deny_all_drops_everything() {
    let mut tools = vec![mock_tool("shell"), mock_tool("reasoning_subagent")];
    let policy = TestPolicy {
        allowed_tools: Some(vec![]),
        ..TestPolicy::default()
    };

    super::apply_policy_tool_filter(&mut tools, Some(&policy), None);
    assert!(
        tools.is_empty(),
        "Some(vec![]) on policy must deny every tool"
    );
}

// ── capture_llm_messages tests ────────────────────────────────

#[test]
fn eager_mcp_policy_allows_only_names_that_pass_policy_and_caller_gates() {
    let policy = TestPolicy {
        allowed_tools: Some(vec!["fs__read_file".into(), "slack__post".into()]),
        excluded_tools: Some(vec!["slack__post".into()]),
        // This test is about the interaction of the two gates, which only
        // has something to say while the risk gate auto-admits.
        mcp_discovered_tool_policy: zeroclaw_config::autonomy::McpDiscoveredToolPolicy::AutoAdmit,
        ..TestPolicy::default()
    };
    let caller = vec!["fs__read_file".to_string(), "github__search".to_string()];
    let access_policy = super::mcp_tool_access_policy(&policy, Some(&caller));

    assert!(
        super::eager_mcp_tool_allowed("fs__read_file", access_policy.as_ref()),
        "name admitted by both policy and caller gates must be registered eagerly"
    );
    assert!(
        !super::eager_mcp_tool_allowed("slack__post", access_policy.as_ref()),
        "policy excluded_tools must block eager MCP registration"
    );
    // `github__search` is in the caller list AND its `__` prefix triggers
    // the risk-profile MCP auto-admit, so both independent gates pass it.
    assert!(
        super::eager_mcp_tool_allowed("github__search", access_policy.as_ref()),
        "name auto-admitted by risk-profile MCP exception and listed by \
         the caller must be registered eagerly"
    );
}

#[test]
fn eager_mcp_policy_caller_per_run_list_does_not_leak_mcp_auto_admit() {
    // Risk profile is permissive enough that the MCP auto-admit
    // fires on its own gate (a non-empty list activates the `__`
    // exception). It also covers `cron_add` so the per-run narrowing
    // can pin that tool through both gates.
    let policy = TestPolicy {
        allowed_tools: Some(vec!["cron_add".into(), "fs__read_file".into()]),
        ..TestPolicy::default()
    };
    // Caller-supplied per-run list narrows to a single non-MCP tool.
    let caller = vec!["cron_add".to_string()];
    let access_policy = super::mcp_tool_access_policy(&policy, Some(&caller));

    assert!(
        super::eager_mcp_tool_allowed("cron_add", access_policy.as_ref()),
        "explicitly-narrowed tool must be admitted by both gates"
    );
    assert!(
        !super::eager_mcp_tool_allowed("filesystem__write_file", access_policy.as_ref()),
        "MCP wrapper outside the caller-supplied per-run list must be \
         rejected — the per-run gate does not honor the risk-profile \
         MCP auto-admit exception (PR #7547 review regression)"
    );
    assert!(
        !super::eager_mcp_tool_allowed("fs__read_file", access_policy.as_ref()),
        "even risk-profile-listed MCP names are rejected when the \
         caller-supplied per-run list does not include them"
    );
}

#[test]
fn deferred_mcp_caller_per_run_list_does_not_leak_mcp_auto_admit() {
    let policy = TestPolicy {
        allowed_tools: Some(vec!["cron_add".into(), "fs__read_file".into()]),
        ..TestPolicy::default()
    };
    let caller = vec!["cron_add".to_string()];
    let access_policy = super::mcp_tool_access_policy(&policy, Some(&caller));

    // The two stubs are MCP-shaped wrappers that the risk-profile
    // auto-admit would otherwise pass, but the caller-supplied
    // per-run list does not include either of them.
    assert_eq!(
        super::mcp_allowed_tool_count(
            ["filesystem__write_file", "github__search"],
            access_policy.as_ref()
        ),
        0,
        "deferred MCP `tool_search` must not be registered when the \
         caller-supplied per-run list admits no MCP stub (PR #7547 \
         review regression)"
    );
}

#[test]
fn eager_mcp_policy_uses_security_policy_without_caller_gate_on_process_message() {
    let policy = TestPolicy {
        allowed_tools: Some(vec!["fs__read_file".into()]),
        mcp_discovered_tool_policy: zeroclaw_config::autonomy::McpDiscoveredToolPolicy::AutoAdmit,
        ..TestPolicy::default()
    };
    let access_policy = super::mcp_tool_access_policy(&policy, None);

    assert!(
        super::eager_mcp_tool_allowed("fs__read_file", access_policy.as_ref()),
        "process_message eager MCP should use the agent SecurityPolicy allowlist"
    );
    // Under `auto_admit`, `github__search` rides its `__` shape in even
    // though nobody listed it.
    assert!(
        super::eager_mcp_tool_allowed("github__search", access_policy.as_ref()),
        "runtime-discovered MCP tools are auto-admitted under auto_admit"
    );
}

/// The default posture on the same path: eager registration must not
/// admit an MCP tool the profile never named. This is the gap that let a
/// server hand the agent a new tool between restarts.
#[test]
fn eager_mcp_policy_rejects_unlisted_mcp_names_under_explicit_only() {
    let policy = TestPolicy {
        allowed_tools: Some(vec!["fs__read_file".into()]),
        ..TestPolicy::default()
    };
    let access_policy = super::mcp_tool_access_policy(&policy, None);

    assert!(
        super::eager_mcp_tool_allowed("fs__read_file", access_policy.as_ref()),
        "a listed MCP tool must still register eagerly"
    );
    assert!(
        !super::eager_mcp_tool_allowed("github__search", access_policy.as_ref()),
        "an unlisted MCP tool must not register eagerly by default"
    );
}

#[test]
fn deferred_mcp_allowed_count_honors_deny_all_policy() {
    let policy = TestPolicy {
        allowed_tools: Some(vec![]),
        ..TestPolicy::default()
    };
    let access_policy = super::mcp_tool_access_policy(&policy, None);

    assert_eq!(
        super::mcp_allowed_tool_count(["fs__read_file", "github__search"], access_policy.as_ref()),
        0,
        "deferred MCP must not register tool_search when policy admits no MCP stubs"
    );
}

#[test]
fn pinned_section_survives_deferred_loading_reassignment() {
    let pinned = "## Pinned MCP Resources\n\n\
        <mcp-resource server=\"docs\" uri=\"docs__file:///handbook.md\" \
        mime=\"text/plain\" trust=\"untrusted-external\">\nhandbook body\n</mcp-resource>\n";

    // Emulate the production statement order for the deferred path.
    // (build_pinned_resources_section result is captured first in `run()`)
    let pinned_section = pinned.to_string();
    // deferred_loading == true branch reassigns the section (as
    // build_deferred_tools_section_filtered does), dropping prior content.
    let mut deferred_section = "## Deferred MCP Tools\n\n- mcp__example".to_string();
    // The fix: append pins AFTER the branch.
    super::append_pinned_mcp_section(&mut deferred_section, &pinned_section);

    assert!(
        deferred_section.contains("## Pinned MCP Resources"),
        "pinned section must survive the deferred-loading reassignment, got: {deferred_section}"
    );
    assert!(
        deferred_section.contains("trust=\"untrusted-external\""),
        "provenance-wrapped pinned content must reach the prompt under deferred_loading"
    );
    assert!(
        deferred_section.contains("## Deferred MCP Tools"),
        "the deferred tools section must also remain present"
    );
}

#[test]
fn append_pinned_mcp_section_is_noop_for_empty() {
    let mut section = "## Deferred MCP Tools".to_string();
    super::append_pinned_mcp_section(&mut section, "");
    assert_eq!(
        section, "## Deferred MCP Tools",
        "empty pinned section must not alter the accumulator"
    );
}

#[test]
fn register_eager_mcp_tool_filters_tools_by_policy() {
    let policy = TestPolicy {
        allowed_tools: Some(vec!["fs__read_file".into()]),
        excluded_tools: Some(vec!["slack__post".into()]),
        // A name that gets in without being listed keeps exercising the
        // auto-admit path independently of the allowlist.
        mcp_discovered_tool_policy: zeroclaw_config::autonomy::McpDiscoveredToolPolicy::AutoAdmit,
        ..TestPolicy::default()
    };
    let access_policy = super::mcp_tool_access_policy(&policy, None);
    let mut tools: Vec<Box<dyn TestTool>> = Vec::new();

    assert!(super::register_eager_mcp_tool_if_allowed(
        mock_tool_arc("fs__read_file"),
        &mut tools,
        access_policy.as_ref(),
    ));
    // github__search contains "__" → auto-admitted
    assert!(super::register_eager_mcp_tool_if_allowed(
        mock_tool_arc("github__search"),
        &mut tools,
        access_policy.as_ref(),
    ));
    // slack__post is explicitly excluded → denied
    assert!(!super::register_eager_mcp_tool_if_allowed(
        mock_tool_arc("slack__post"),
        &mut tools,
        access_policy.as_ref(),
    ));

    assert_eq!(tool_names(&tools), vec!["fs__read_file", "github__search"]);
}

// ── agent_provider_composite regression ───────────────────────────────

#[test]
fn agent_provider_composite_returns_dotted_ref_not_bare_family() {
    use zeroclaw_config::providers::ModelProviderRef;
    use zeroclaw_config::schema::{
        AliasedAgentConfig, ModelProviderConfig, OpenAIModelProviderConfig,
    };

    let alias = "qwertfoozp";

    let mut config = zeroclaw_config::schema::Config::default();
    config.providers.models.openai.insert(
        alias.to_string(),
        OpenAIModelProviderConfig {
            base: ModelProviderConfig {
                requires_openai_auth: true,
                ..Default::default()
            },
        },
    );
    config.agents.insert(
        "my_agent".to_string(),
        AliasedAgentConfig {
            model_provider: ModelProviderRef::new(format!("openai.{alias}")),
            ..Default::default()
        },
    );

    let result = super::agent_provider_composite(&config, "my_agent");

    // Must be the full dotted ref so the alias-aware factory path is taken.
    assert_eq!(
        result.as_deref(),
        Some("openai.qwertfoozp"),
        "agent_provider_composite must return the dotted composite ref"
    );
    // Explicitly assert it is NOT the bare family — this is the regression
    // this test protects against.
    assert_ne!(
        result.as_deref(),
        Some("openai"),
        "bare family name would bypass the alias-aware factory path and drop \
         requires_openai_auth from the config, routing to the wrong provider"
    );
}

#[test]
fn direct_turn_agent_config_resolves_runtime_profile_tunables() {
    use zeroclaw_config::providers::RuntimeProfileRef;
    use zeroclaw_config::schema::{AliasedAgentConfig, RuntimeProfileConfig};

    let mut config = zeroclaw_config::schema::Config::default();
    config.runtime_profiles.insert(
        "long".to_string(),
        RuntimeProfileConfig {
            max_tool_iterations: 25,
            ..Default::default()
        },
    );
    config.agents.insert(
        "default".to_string(),
        AliasedAgentConfig {
            runtime_profile: RuntimeProfileRef::new("long"),
            ..Default::default()
        },
    );

    let agent = super::resolved_agent_for_turn(&config, "default")
        .expect("configured agent should resolve for direct turns");

    assert_eq!(
        agent.resolved.max_tool_iterations, 25,
        "direct agent turns must honor runtime_profiles.*.max_tool_iterations \
         instead of the skipped AliasedAgentConfig::resolved default"
    );
}

#[tokio::test]
async fn runtime_entrypoints_resolve_runtime_profile_tunables_before_provider_setup() {
    use zeroclaw_config::providers::RuntimeProfileRef;
    use zeroclaw_config::schema::{AliasedAgentConfig, RuntimeProfileConfig};

    let mut config = zeroclaw_config::schema::Config::default();
    config.runtime_profiles.insert(
        "entrypoint-profile".to_string(),
        RuntimeProfileConfig {
            max_tool_iterations: 3,
            ..Default::default()
        },
    );
    config.agents.insert(
        "entrypoint-profile-agent".to_string(),
        AliasedAgentConfig {
            runtime_profile: RuntimeProfileRef::new("entrypoint-profile"),
            ..Default::default()
        },
    );

    let seen = Arc::new(Mutex::new(Vec::<(String, usize)>::new()));
    let seen_for_hook = Arc::clone(&seen);
    {
        let mut hook = RESOLVED_AGENT_FOR_TURN_TEST_HOOK
            .lock()
            .expect("resolved-agent test hook lock should not be poisoned");
        *hook = Some(Arc::new(move |alias, max_tool_iterations| {
            seen_for_hook
                .lock()
                .expect("seen lock should not be poisoned")
                .push((alias.to_string(), max_tool_iterations));
        }));
    }

    let _ = super::run(
        config.clone(),
        "entrypoint-profile-agent",
        Some("hello".to_string()),
        None,
        None,
        None,
        false,
        None,
        None,
        TurnOrigin::SubTurn,
        super::AgentRunOverrides::default(),
    )
    .await;
    let _ = super::process_message(
        config,
        "entrypoint-profile-agent",
        "hello",
        Some("session"),
        TurnOrigin::SubTurn,
    )
    .await;

    {
        let mut hook = RESOLVED_AGENT_FOR_TURN_TEST_HOOK
            .lock()
            .expect("resolved-agent test hook lock should not be poisoned");
        *hook = None;
    }

    let seen = seen.lock().expect("seen lock should not be poisoned");
    let matching = seen
        .iter()
        .filter(|(alias, max_tool_iterations)| {
            alias == "entrypoint-profile-agent" && *max_tool_iterations == 3
        })
        .count();

    assert_eq!(
        matching, 2,
        "run and process_message must both resolve runtime_profiles.*.max_tool_iterations \
         before provider setup; observed {seen:?}"
    );
}
#[tokio::test]
async fn process_message_seam_narrows_safe_defaults_outside_allowed_tools() {
    let config = zeroclaw_config::schema::Config::default();
    let security = Arc::new(TestPolicy {
        workspace_dir: std::env::temp_dir(),
        ..TestPolicy::default()
    });
    let risk = zeroclaw_config::schema::RiskProfileConfig::default();
    let mem: Arc<dyn zeroclaw_memory::Memory> = Arc::new(zeroclaw_memory::NoneMemory::new("test"));

    // Build the real eager built-in registry, as process_message does.
    let built = crate::tools::all_tools(
        Arc::new(config.clone()),
        &security,
        &risk,
        "test",
        mem,
        &config.browser,
        &config.http_request,
        &config.web_fetch,
        &security.workspace_dir,
        &config.agents,
        None,
        &config,
        None,
    );

    let before = tool_names(&built.tools);
    assert!(
        before.contains(&"web_search_tool"),
        "precondition: web_search_tool in the eager registry, got {before:?}"
    );
    assert!(
        before.contains(&"shell"),
        "precondition: shell in the eager registry, got {before:?}"
    );

    // Restrict allowed_tools to shell only, at non-Full autonomy - the exact
    // scenario where the removed admit diverged from the plain filter.
    let policy = Arc::new(TestPolicy {
        workspace_dir: std::env::temp_dir(),
        allowed_tools: Some(vec!["shell".into()]),
        autonomy: crate::security::AutonomyLevel::Supervised,
        ..TestPolicy::default()
    });

    let assembled =
        crate::tools::scoped::ScopedToolRegistry::assemble(crate::tools::scoped::ScopedAssembly {
            config: &config,
            agent_alias: "test",
            security: &policy,
            built,
            skills: &[],
            runtime: Arc::new(crate::platform::NativeRuntime::new()),
            caller_allowed: None, // process_message has no caller allowlist
            connect_mcp: false,   // exercise the filter without MCP fixtures
            exclude_memory: false,
            list_deferred_mcp_specs: false,
            emit_assembly_logs: false,
            mcp_registry: None,
        })
        .await;

    let filtered: Vec<&str> = assembled.registry.iter().map(|t| t.name()).collect();
    assert!(
        !filtered.contains(&"web_search_tool"),
        "unified filter must DROP a read-only default outside allowed_tools \
         (the removed safe-defaults admit retained it), got {filtered:?}"
    );
    assert!(
        filtered.contains(&"shell"),
        "shell in allowed_tools must survive, got {filtered:?}"
    );
}

// ── Observer metadata regression tests ──

#[derive(Default)]
struct CapturingObserver {
    events: parking_lot::Mutex<Vec<ObserverEvent>>,
}

impl Observer for CapturingObserver {
    fn record_event(&self, event: &ObserverEvent) {
        self.events.lock().push(event.clone());
    }
    fn record_metric(&self, _metric: &zeroclaw_api::observability_traits::ObserverMetric) {}
    fn name(&self) -> &str {
        "capturing"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn flush(&self) {}
}

fn assert_all_events_share_turn_id(
    events: &[ObserverEvent],
    expected_alias: Option<&str>,
    expected_channel: Option<&str>,
) {
    let mut turn_ids: Vec<String> = Vec::new();
    for event in events {
        let (variant, channel, agent_alias, turn_id) = match event {
            ObserverEvent::AgentStart {
                channel,
                agent_alias,
                turn_id,
                ..
            } => ("AgentStart", channel, agent_alias, turn_id),
            ObserverEvent::AgentEnd {
                channel,
                agent_alias,
                turn_id,
                ..
            } => ("AgentEnd", channel, agent_alias, turn_id),
            ObserverEvent::LlmRequest {
                channel,
                agent_alias,
                turn_id,
                ..
            } => ("LlmRequest", channel, agent_alias, turn_id),
            ObserverEvent::LlmResponse {
                channel,
                agent_alias,
                turn_id,
                ..
            } => ("LlmResponse", channel, agent_alias, turn_id),
            ObserverEvent::ToolCallStart {
                channel,
                agent_alias,
                turn_id,
                ..
            } => ("ToolCallStart", channel, agent_alias, turn_id),
            ObserverEvent::ToolCall {
                channel,
                agent_alias,
                turn_id,
                ..
            } => ("ToolCall", channel, agent_alias, turn_id),
            _ => continue,
        };
        assert!(
            channel.is_some(),
            "{variant} observer event must carry channel, got None: {event:?}"
        );
        assert!(
            agent_alias.is_some(),
            "{variant} observer event must carry agent_alias, got None: {event:?}"
        );
        assert!(
            turn_id.is_some(),
            "{variant} observer event must carry turn_id, got None: {event:?}"
        );
        turn_ids.push(turn_id.clone().expect("checked Some above"));
    }

    assert!(!turn_ids.is_empty(), "expected turn events with turn_id");
    let first = &turn_ids[0];
    assert!(
        turn_ids.iter().all(|id| id == first),
        "all turn_ids should be consistent"
    );

    if let Some(alias) = expected_alias {
        for e in events {
            let agent_alias = match e {
                ObserverEvent::AgentStart { agent_alias, .. }
                | ObserverEvent::AgentEnd { agent_alias, .. }
                | ObserverEvent::LlmRequest { agent_alias, .. }
                | ObserverEvent::LlmResponse { agent_alias, .. }
                | ObserverEvent::ToolCallStart { agent_alias, .. }
                | ObserverEvent::ToolCall { agent_alias, .. } => agent_alias,
                _ => continue,
            };
            assert_eq!(
                agent_alias.as_deref(),
                Some(alias),
                "agent_alias should be consistent"
            );
        }
    }

    if let Some(channel) = expected_channel {
        for e in events {
            let ch = match e {
                ObserverEvent::AgentStart { channel: ch, .. }
                | ObserverEvent::LlmRequest { channel: ch, .. }
                | ObserverEvent::LlmResponse { channel: ch, .. }
                | ObserverEvent::ToolCallStart { channel: ch, .. }
                | ObserverEvent::ToolCall { channel: ch, .. }
                | ObserverEvent::AgentEnd { channel: ch, .. } => ch,
                _ => continue,
            };
            assert_eq!(ch.as_deref(), Some(channel), "channel should be consistent");
        }
    }
}

#[tokio::test]
async fn run_tool_call_loop_events_share_consistent_turn_id_channel_and_alias() {
    use super::run_tool_call_loop;

    let turn_id = uuid::Uuid::new_v4().to_string();
    let invocations = Arc::new(AtomicUsize::new(0));
    let model_provider = ScriptedModelProvider::from_text_responses(vec![
        r#"<tool_call>
{"name":"count_tool","arguments":{"value":"X"}}
</tool_call>"#,
        "done",
    ]);

    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
        "count_tool",
        Arc::clone(&invocations),
    ))];

    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();
    let mut history = vec![ChatMessage::system("test"), ChatMessage::user("hello")];

    let result = run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        exec: ResolvedAgentExecution {
            task_owner: None,
            model_access: ResolvedModelAccess {
                model_provider: &model_provider,
                provider_name: "mock-provider",
                model: "mock-model",
                temperature: Some(0.0),
            },
            tools_registry: &tools_registry,
            observer: observer.as_ref(),
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 10,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 0,
            keep_tool_context_turns: 2,
            context_token_budget: 0,
            receipt_generator: None,
            knobs: &LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: Some("test-agent"),
        turn_id: &turn_id,
    })
    .await
    .expect("tool loop should succeed");

    assert_eq!(result, "done");

    let events = capturing.events.lock();
    assert_all_events_share_turn_id(&events, Some("test-agent"), Some("cli"));
}

#[tokio::test]
async fn agent_turn_propagates_resolved_agent_alias_to_observer_events() {
    // Regression guard: process_message resolves agent_alias but
    // agent_turn hardcoded `agent_alias: None` in the ToolLoop it built,
    // so every lifecycle event on this path violated the
    // (channel, agent_alias, turn_id) contract that
    // assert_all_events_share_turn_id enforces.
    let model_provider = ScriptedModelProvider::from_text_responses(vec![
        r#"<tool_call>
{"name":"count_tool","arguments":{"value":"X"}}
</tool_call>"#,
        "done",
    ]);
    let invocations = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
        "count_tool",
        Arc::clone(&invocations),
    ))];
    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();
    let mut history = vec![ChatMessage::system("test"), ChatMessage::user("hello")];

    let result = agent_turn(
        None,
        &model_provider,
        &mut history,
        &tools_registry,
        observer.as_ref(),
        "mock-provider",
        "mock-model",
        Some(0.0),
        true,
        "daemon",
        None,
        &zeroclaw_config::schema::MultimodalConfig::default(),
        4,
        None,
        &[],
        &[],
        None,
        None,
        false,
        false,
        0,
        0,
        None,
        TurnOrigin::SubTurn,
        None,
        Some("test-agent"),
        None, // turn_id: self-minted
        None,
    )
    .await
    .expect("agent_turn should complete");

    assert_eq!(result, "done");
    assert_eq!(invocations.load(Ordering::SeqCst), 1);

    let events = capturing.events.lock();
    assert_all_events_share_turn_id(&events, Some("test-agent"), Some("daemon"));
}

/// `agent_turn` (the `process_message` path: gateway webhook chat and
/// peer messages) must bracket every turn with exactly one
/// `AgentStart`/`AgentEnd` pair carrying the same `(channel, agent_alias,
/// turn_id)` triple as the inner engine events. Regression guard for the
/// fix where channel/daemon turns never emitted these lifecycle events,
/// so `/api/events/history` had `llm_request` but no
/// `agent_start`/`agent_end`.
#[tokio::test]
async fn agent_turn_brackets_turn_with_agent_start_and_agent_end() {
    let model_provider = ScriptedModelProvider::from_text_responses(vec!["done"]);
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let capturing = Arc::new(CapturingObserver::default());
    let mut history = vec![ChatMessage::system("test"), ChatMessage::user("hello")];

    let result = agent_turn(
        None, // config: configless test
        &model_provider,
        &mut history,
        &tools_registry,
        capturing.as_ref(),
        "mock-provider",
        "mock-model",
        Some(0.0),
        true,
        "daemon",
        None,
        &zeroclaw_config::schema::MultimodalConfig::default(),
        4,
        None,
        &[],
        &[],
        None,
        None,
        false,
        false, // parallel_tools
        0,     // max_tool_result_chars: disabled for test
        0,     // context_token_budget: disabled for test
        None,  // channel
        TurnOrigin::SubTurn,
        None,
        Some("test-agent"),
        None, // turn_id: self-minted
        None,
    )
    .await
    .expect("turn should succeed");
    assert_eq!(result, "done");

    let events = capturing.events.lock();
    let scoped = events_for_alias(&events, "test-agent");
    let (starts, ends) = lifecycle_events_for_alias(&events, "test-agent");
    assert_eq!(starts, 1, "exactly one AgentStart, got {scoped:?}");
    assert_eq!(ends, 1, "exactly one AgentEnd, got {scoped:?}");
    assert!(
        matches!(scoped.first(), Some(ObserverEvent::AgentStart { .. })),
        "first event must be AgentStart, got {scoped:?}"
    );
    assert!(
        matches!(scoped.last(), Some(ObserverEvent::AgentEnd { .. })),
        "last event must be AgentEnd, got {scoped:?}"
    );

    // The brackets and the inner engine events they wrap must agree on
    // the full (channel, agent_alias, turn_id) triple — the same
    // expectation every other bracketed entry point is held to.
    assert_all_events_share_turn_id(&events, Some("test-agent"), Some("daemon"));
}

/// When the caller pre-mints a turn id (`process_message` does, so its
/// pre-turn RAG retrieval correlates with the turn), `agent_turn` must
/// bracket the turn with that id instead of minting its own.
#[tokio::test]
async fn agent_turn_uses_the_pre_minted_turn_id_on_its_bracket() {
    // Harness mirrors agent_turn_brackets_turn_with_agent_start_and_agent_end.
    let model_provider = ScriptedModelProvider::from_text_responses(vec!["done"]);
    let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
    let capturing = Arc::new(CapturingObserver::default());
    let mut history = vec![ChatMessage::system("test"), ChatMessage::user("hello")];

    let result = agent_turn(
        None, // config
        &model_provider,
        &mut history,
        &tools_registry,
        capturing.as_ref(),
        "mock-provider",
        "mock-model",
        Some(0.0),
        true,
        "daemon",
        None,
        &zeroclaw_config::schema::MultimodalConfig::default(),
        4,
        None,
        &[],
        &[],
        None,
        None,
        false,
        false, // parallel_tools
        0,     // max_tool_result_chars: disabled for test
        0,     // context_token_budget: disabled for test
        None,  // channel
        TurnOrigin::SubTurn,
        None,
        Some("test-agent"),
        Some("pre-minted-turn"),
        None,
    )
    .await
    .expect("turn should succeed");
    assert_eq!(result, "done");

    let events = capturing.events.lock();
    match events
        .iter()
        .find(|e| matches!(e, ObserverEvent::AgentStart { .. }))
        .expect("agent_turn must emit AgentStart")
    {
        ObserverEvent::AgentStart { turn_id, .. } => {
            assert_eq!(turn_id.as_deref(), Some("pre-minted-turn"));
        }
        _ => unreachable!(),
    }

    // The bracket and the inner engine events must also agree on the
    // full (channel, agent_alias, turn_id) triple with the pre-minted id.
    assert_all_events_share_turn_id(&events, Some("test-agent"), Some("daemon"));
}

/// `run()` (the CLI/daemon direct-turn entry point) was the last
/// production site that open-coded its own `AgentStart`/`AgentEnd`
/// instead of going through `AgentTurnGuard` like every other entry
/// point (`agent_turn`, `Agent::turn`/`turn_streamed`, the
/// orchestrator's `process_channel_message`). Between the open-coded
/// events sat roughly a dozen exit paths (`?`, early `return`) where
/// `AgentStart` fired but `AgentEnd` never did, leaving observers
/// permanently unbalanced. `run()` builds its own observer and model
/// provider internally with no injection seam, so these tests drive it
/// end to end: a real `Config`, the real tool registry, and a local
/// HTTP server standing in for the model. Event capture leans on the
/// process-wide broadcast hook the gateway uses to fan events out to
/// `/api/events` (`observability::set_scoped_broadcast_hook`);
/// `observability::HOOK_TEST_LOCK` serializes against
/// `observability::mod`'s own hook tests so neither steals the other's
/// events off the single global hook slot.
// `config.providers.models.ollama.*` dispatches through the OpenAI-
// compatible wire (`OllamaModelProviderConfig::create_provider` builds an
// `OpenAiCompatibleModelProvider` labeled "Ollama"), so the scripted
// response is OpenAI's `choices[0].message.content` shape at
// `/v1/chat/completions`, not the native `/api/chat` shape.
async fn respond_with_done() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "choices": [{"message": {"content": "done"}}]
    }))
}

/// The observer registry is process-global, so a concurrently running test's
/// events interleave into this stream. Every positional assertion (first,
/// last, ordering) must be made on the calling test's own agent-scoped
/// sub-stream, never on the raw global stream.
fn events_for_alias<'a>(events: &'a [ObserverEvent], alias: &str) -> Vec<&'a ObserverEvent> {
    events
        .iter()
        .filter(|e| match e {
            ObserverEvent::AgentStart { agent_alias, .. }
            | ObserverEvent::AgentEnd { agent_alias, .. }
            | ObserverEvent::LlmRequest { agent_alias, .. }
            | ObserverEvent::LlmResponse { agent_alias, .. } => {
                agent_alias.as_deref() == Some(alias)
            }
            _ => false,
        })
        .collect()
}

fn lifecycle_events_for_alias(events: &[ObserverEvent], alias: &str) -> (usize, usize) {
    let starts = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                ObserverEvent::AgentStart { agent_alias, .. }
                    if agent_alias.as_deref() == Some(alias)
            )
        })
        .count();
    let ends = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                ObserverEvent::AgentEnd { agent_alias, .. }
                    if agent_alias.as_deref() == Some(alias)
            )
        })
        .count();
    (starts, ends)
}

/// A `Config::default()` rooted under a fresh temp dir instead of the
/// real `$HOME/.zeroclaw`. `Config::default()` points `data_dir` (the
/// tachi memory db, `{data_dir}/memory/tachi.db`) and `config_path`
/// (whose parent `install_root_dir()` backs `agent_workspace_dir`, i.e.
/// `{install_root}/agents/<alias>/workspace`) at the real home
/// directory, so a `run()`-driving test that doesn't override them
/// creates/writes real files on every dev and CI machine and, under the
/// crate's parallel test gate, has every such test contend on the same
/// `tachi.db`. The returned `TempDir` must be kept alive for the
/// duration of the test — it deletes the directory on drop.
fn isolated_run_test_config() -> (tempfile::TempDir, Config) {
    let tmp = tempdir().expect("temp dir for isolated run() test config should create");
    let mut config = Config {
        data_dir: tmp.path().join("data"),
        config_path: tmp.path().join("config.toml"),
        ..Config::default()
    };
    if !zeroclaw_memory::TACHI_COMPILED {
        config.memory.backend = "markdown".into();
    }
    (tmp, config)
}

#[tokio::test]
async fn run_brackets_successful_turn_with_agent_start_and_agent_end() {
    use axum::{Router, routing::post};
    use tokio::net::TcpListener;
    use zeroclaw_config::schema::{
        AliasedAgentConfig, ModelProviderConfig, OllamaModelProviderConfig, RiskProfileConfig,
    };

    let _hook_lock = observability::HOOK_TEST_LOCK.lock().await;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let addr = listener.local_addr().expect("listener should have address");
    let app = Router::new().route("/v1/chat/completions", post(respond_with_done));
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("test server should run");
    });

    let (_tmp, mut config) = isolated_run_test_config();
    config.providers.models.ollama.insert(
        "default".to_string(),
        OllamaModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("run-lifecycle-success-model".to_string()),
                timeout_secs: Some(5),
                uri: Some(format!("http://{addr}")),
                ..Default::default()
            },
        },
    );
    config.agents.insert(
        "run-lifecycle-success-agent".to_string(),
        AliasedAgentConfig {
            model_provider: "ollama.default".into(),
            risk_profile: "default".into(),
            ..Default::default()
        },
    );
    config
        .risk_profiles
        .insert("default".to_string(), RiskProfileConfig::default());

    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();
    let hook_guard = observability::set_scoped_broadcast_hook(observer);

    let result = super::run(
        config,
        "run-lifecycle-success-agent",
        Some("hello".to_string()),
        None,
        None,
        None,
        false,
        None,
        None,
        TurnOrigin::SubTurn,
        super::AgentRunOverrides::default(),
    )
    .await;

    drop(hook_guard);
    server.abort();

    let output = result.expect("run() should complete the scripted turn");
    assert_eq!(output, "done");

    let events = capturing.events.lock();
    let (starts, ends) = lifecycle_events_for_alias(&events, "run-lifecycle-success-agent");
    let scoped = events_for_alias(&events, "run-lifecycle-success-agent");
    assert_eq!(starts, 1, "exactly one AgentStart, got {scoped:?}");
    assert_eq!(ends, 1, "exactly one AgentEnd, got {scoped:?}");
    assert!(
        matches!(scoped.first(), Some(ObserverEvent::AgentStart { .. })),
        "first event must be AgentStart, got {scoped:?}"
    );
    assert!(
        matches!(scoped.last(), Some(ObserverEvent::AgentEnd { .. })),
        "last event must be AgentEnd, got {scoped:?}"
    );
}

/// The core regression this fix addresses: before it, a model-call
/// failure inside `run()` propagated out through `?` while the
/// open-coded `AgentStart` had already fired, so `AgentEnd` never did.
/// With `AgentTurnGuard` in place, `Drop` closes the bracket on this
/// early-return path too.
#[tokio::test]
async fn run_still_closes_the_bracket_when_the_model_call_fails() {
    use zeroclaw_config::schema::{
        AliasedAgentConfig, ModelProviderConfig, OllamaModelProviderConfig, RiskProfileConfig,
    };

    let _hook_lock = observability::HOOK_TEST_LOCK.lock().await;

    // Bind an ephemeral port and immediately drop the listener: any
    // connection attempt against it fails fast and deterministically
    // (connection refused) without depending on a hardcoded "probably
    // unused" port number.
    let addr = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener should bind");
        listener.local_addr().expect("listener should have address")
    };

    let (_tmp, mut config) = isolated_run_test_config();
    config.providers.models.ollama.insert(
        "default".to_string(),
        OllamaModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("run-lifecycle-error-model".to_string()),
                timeout_secs: Some(5),
                uri: Some(format!("http://{addr}")),
                ..Default::default()
            },
        },
    );
    config.agents.insert(
        "run-lifecycle-error-agent".to_string(),
        AliasedAgentConfig {
            model_provider: "ollama.default".into(),
            risk_profile: "default".into(),
            ..Default::default()
        },
    );
    config
        .risk_profiles
        .insert("default".to_string(), RiskProfileConfig::default());

    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();
    let hook_guard = observability::set_scoped_broadcast_hook(observer);

    let result = super::run(
        config,
        "run-lifecycle-error-agent",
        Some("hello".to_string()),
        None,
        None,
        None,
        false,
        None,
        None,
        TurnOrigin::SubTurn,
        super::AgentRunOverrides::default(),
    )
    .await;

    drop(hook_guard);

    assert!(
        result.is_err(),
        "run() should surface the model provider's connection failure, got {result:?}"
    );

    let events = capturing.events.lock();
    let (starts, ends) = lifecycle_events_for_alias(&events, "run-lifecycle-error-agent");
    assert_eq!(
        starts, 1,
        "exactly one AgentStart even on the error path, got {events:?}"
    );
    assert_eq!(
        ends, 1,
        "AgentEnd must still fire via Drop on the early-return error path \
         (the regression this fix closes), got {events:?}"
    );
    let scoped = events_for_alias(&events, "run-lifecycle-error-agent");
    assert!(
        matches!(scoped.first(), Some(ObserverEvent::AgentStart { .. })),
        "first event must be AgentStart, got {scoped:?}"
    );
    assert!(
        matches!(scoped.last(), Some(ObserverEvent::AgentEnd { .. })),
        "last event must be AgentEnd even on the error path, got {scoped:?}"
    );
}

/// A `model_switch` tool call from the model must be rejected — the tool is
/// retired from the ordinary registry, so the loop answers "Unknown tool"
/// instead of switching — and the run must still complete with exactly one
/// balanced lifecycle pair, attributed to the ORIGINAL route. Runtime model
/// switching belongs to the trusted channel `/model` command path, which
/// applies the route outside the tool loop.
#[tokio::test]
async fn run_rejects_model_switch_tool_call_and_keeps_original_route() {
    use axum::{Json, Router, extract::State, routing::post};
    use tokio::net::TcpListener;
    use zeroclaw_config::schema::{
        AliasedAgentConfig, ModelProviderConfig, OllamaModelProviderConfig, RiskProfileConfig,
    };

    let _hook_lock = observability::HOOK_TEST_LOCK.lock().await;

    // First response: a native tool call requesting a switch to
    // `ollama.switched`. Second response (only reachable once the
    // switched-to provider is actually in use): plain "done".
    type CallCount = Arc<std::sync::atomic::AtomicUsize>;

    async fn respond_switch_then_done(State(calls): State<CallCount>) -> Json<serde_json::Value> {
        let call = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            Json(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": null,
                        "tool_calls": [{
                            "id": "call-switch-1",
                            "type": "function",
                            "function": {
                                "name": "model_switch",
                                "arguments": "{\"action\":\"set\",\"model_provider\":\"ollama.switched\",\"model\":\"switched-model\"}"
                            }
                        }]
                    }
                }]
            }))
        } else {
            Json(serde_json::json!({
                "choices": [{"message": {"content": "done"}}]
            }))
        }
    }

    let calls: CallCount = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let addr = listener.local_addr().expect("listener should have address");
    let app = Router::new()
        .route("/v1/chat/completions", post(respond_switch_then_done))
        .with_state(calls);
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("test server should run");
    });

    let (_tmp, mut config) = isolated_run_test_config();
    for alias in ["default", "switched"] {
        config.providers.models.ollama.insert(
            alias.to_string(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some(format!("run-lifecycle-switch-{alias}-model")),
                    timeout_secs: Some(5),
                    uri: Some(format!("http://{addr}")),
                    ..Default::default()
                },
            },
        );
    }
    config.agents.insert(
        "run-lifecycle-switch-agent".to_string(),
        AliasedAgentConfig {
            model_provider: "ollama.default".into(),
            risk_profile: "default".into(),
            ..Default::default()
        },
    );
    config
        .risk_profiles
        .insert("default".to_string(), RiskProfileConfig::default());

    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();
    let hook_guard = observability::set_scoped_broadcast_hook(observer);

    let result = super::run(
        config,
        "run-lifecycle-switch-agent",
        Some("please switch models".to_string()),
        None,
        None,
        None,
        false,
        None,
        None,
        TurnOrigin::SubTurn,
        super::AgentRunOverrides::default(),
    )
    .await;

    drop(hook_guard);
    server.abort();

    let output = result.expect("run() should complete after the rejected switch request");
    assert_eq!(output, "done");

    let events = capturing.events.lock();
    let scoped = events_for_alias(&events, "run-lifecycle-switch-agent");
    let (starts, ends) = lifecycle_events_for_alias(&events, "run-lifecycle-switch-agent");
    assert_eq!(
        starts, 1,
        "a rejected switch must not open a second bracket, got {events:?}"
    );
    assert_eq!(
        ends, 1,
        "a rejected switch must still close with exactly one AgentEnd, got {events:?}"
    );
    assert!(
        matches!(scoped.first(), Some(ObserverEvent::AgentStart { .. })),
        "first event must be AgentStart, got {scoped:?}"
    );
    assert!(
        matches!(scoped.last(), Some(ObserverEvent::AgentEnd { .. })),
        "last event must be AgentEnd, got {scoped:?}"
    );

    // The rejected request must surface as a failed ToolCall, never a route
    // change: the loop kept the original provider for the follow-up call.
    let switch_call = events.iter().find_map(|e| match e {
        ObserverEvent::ToolCall {
            tool,
            success,
            result,
            ..
        } if tool == "model_switch" => Some((*success, result.clone())),
        _ => None,
    });
    let (switch_success, switch_result) =
        switch_call.expect("the model_switch tool call should be observable");
    assert!(
        !switch_success,
        "the retired model_switch tool must fail, got {events:?}"
    );
    assert!(
        switch_result.unwrap_or_default().contains("Unknown tool"),
        "the failure must be an unknown-tool rejection, got {events:?}"
    );

    let end_route = events
        .iter()
        .find_map(|e| match e {
            ObserverEvent::AgentEnd {
                agent_alias,
                model_provider,
                model,
                ..
            } if agent_alias.as_deref() == Some("run-lifecycle-switch-agent") => {
                Some((model_provider.clone(), model.clone()))
            }
            _ => None,
        })
        .expect("AgentEnd for the switch agent should be present");
    assert_eq!(
        end_route,
        (
            "ollama.default".to_string(),
            "run-lifecycle-switch-default-model".to_string()
        ),
        "AgentEnd must stay attributed to the original route — the model \
         cannot switch its own route through a tool call, got {events:?}"
    );
}

#[tokio::test]
async fn recall_uses_canonical_memory_scopes_in_sequential_and_parallel_turns() {
    if !zeroclaw_memory::TACHI_COMPILED {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "tachi".into(),
        hygiene_enabled: false,
        embedding_provider: "none".into(),
        ..Default::default()
    };
    let mem: Arc<dyn zeroclaw_memory::Memory> =
        Arc::from(zeroclaw_memory::create_memory(&cfg, dir.path(), None).unwrap());
    for session in ["A", "C", "gw_A", "B", "sender", "history"] {
        mem.store(
            session,
            &format!("fixture-{session}-secret"),
            zeroclaw_memory::MemoryCategory::Conversation,
            Some(session),
        )
        .await
        .unwrap();
    }
    mem.store(
        "global",
        "fixture-global-fact",
        zeroclaw_memory::MemoryCategory::Core,
        None,
    )
    .await
    .unwrap();
    for (scopes, parallel_tools) in [
        (Some(vec!["A"]), false),
        (Some(vec!["C"]), true),
        (Some(vec!["sender", "history"]), true),
        (None, false),
    ] {
        let scopes = scopes.as_deref();
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
            r#"<tool_call>{"name":"memory_recall","arguments":{"query":"*","limit":20}}</tool_call>
<tool_call>{"name":"memory_recall","arguments":{"query":"fixture","limit":20}}</tool_call>"#,
            "done",
        ]);
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(
            crate::tools::MemoryRecallTool::new(Arc::clone(&mem)),
        )];
        let mut history = vec![ChatMessage::user("recall fixture")];
        let turn_id = uuid::Uuid::new_v4().to_string();
        let multimodal = zeroclaw_config::schema::MultimodalConfig::default();
        let pacing = zeroclaw_config::schema::PacingConfig::default();
        let knobs = LoopKnobs::default();
        let future = run_tool_call_loop(ToolLoop {
            parent_agent_alias: None,
            exec: ResolvedAgentExecution {
                task_owner: None,
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "scripted",
                    model: "scripted-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &NoopObserver,
                silent: true,
                approval: None,
                multimodal_config: &multimodal,
                config: None,
                max_tool_iterations: 3,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &pacing,
                strict_tool_parsing: false,
                parallel_tools,
                max_tool_result_chars: 0,
                keep_tool_context_turns: 2,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &knobs,
            },
            history: &mut history,
            channel_name: "wss",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            memory: scopes.map(|scopes| crate::agent::memory_inject::TurnMemory {
                handle: mem.as_ref(),
                query: "fixture".into(),
                sessions: scopes.iter().map(|scope| Some((*scope).into())).collect(),
                suppress: true,
                cfg: crate::agent::memory_inject::MemoryInjectConfig::default(),
            }),
            ingress: IngressContext::agent_direct(),
            agent_alias: None,
            turn_id: &turn_id,
        });
        // This parent scope must be replaced even when the child has no memory.
        zeroclaw_api::TOOL_LOOP_MEMORY_SESSIONS
            .scope(
                vec!["A".into()],
                scope_session_key(Some("gw_A".into()), future),
            )
            .await
            .expect("recall turn should complete");

        let results = history
            .iter()
            .filter(|m| m.content.starts_with("[Tool results]"))
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(results.contains("fixture-global-fact"), "{results}");
        for source in ["A", "C", "gw_A", "B", "sender", "history"] {
            assert_eq!(
                results.contains(&format!("fixture-{source}-secret")),
                scopes.is_some_and(|allowed| allowed.contains(&source)),
                "{source}: {results}"
            );
        }
    }
}

// Additive scenario regression: the canonical-scope test above remains unchanged.
#[tokio::test]
async fn scheduled_and_delegated_tools_obey_host_memory_policy() {
    if !zeroclaw_memory::TACHI_COMPILED {
        return;
    }
    use zeroclaw_api::ingress::TurnOrigin;
    use zeroclaw_api::memory_traits::MemoryReadPolicy;
    let dir = tempfile::tempdir().unwrap();
    let cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "tachi".into(),
        hygiene_enabled: false,
        embedding_provider: "none".into(),
        ..Default::default()
    };
    let mem: Arc<dyn zeroclaw_memory::Memory> =
        Arc::from(zeroclaw_memory::create_memory(&cfg, dir.path(), None).unwrap());
    mem.store(
        "chat",
        "PRIVATE_CHAT_CANARY",
        zeroclaw_memory::MemoryCategory::Conversation,
        Some("A"),
    )
    .await
    .unwrap();
    mem.store(
        "fact",
        "DURABLE_FACT",
        zeroclaw_memory::MemoryCategory::Core,
        None,
    )
    .await
    .unwrap();
    for origin in [TurnOrigin::Cron, TurnOrigin::Daemon, TurnOrigin::SubTurn] {
        for parallel_tools in [false, true] {
            let model_provider = ScriptedModelProvider::from_text_responses(vec![
                r#"<tool_call>{"name":"memory_recall","arguments":{"query":"*","limit":20}}</tool_call>
<tool_call>{"name":"memory_export","arguments":{"session_id":"A","origin":"interactive"}}</tool_call>"#,
                "done",
            ]);
            let tools_registry: Vec<Box<dyn Tool>> = vec![
                Box::new(crate::tools::MemoryRecallTool::new(Arc::clone(&mem))),
                Box::new(crate::tools::MemoryExportTool::new(Arc::clone(&mem))),
            ];
            let mut history = vec![ChatMessage::user("recall fixture")];
            let turn_id = uuid::Uuid::new_v4().to_string();
            let multimodal = zeroclaw_config::schema::MultimodalConfig::default();
            let pacing = zeroclaw_config::schema::PacingConfig::default();
            let knobs = LoopKnobs::default();
            let future = run_tool_call_loop(ToolLoop {
                parent_agent_alias: None,
                exec: ResolvedAgentExecution {
                    task_owner: None,
                    model_access: ResolvedModelAccess {
                        model_provider: &model_provider,
                        provider_name: "scripted",
                        model: "scripted-model",
                        temperature: Some(0.0),
                    },
                    tools_registry: &tools_registry,
                    observer: &NoopObserver,
                    silent: true,
                    approval: None,
                    multimodal_config: &multimodal,
                    config: None,
                    max_tool_iterations: 3,
                    hooks: None,
                    excluded_tools: &[],
                    dedup_exempt_tools: &[],
                    activated_tools: None,
                    model_switch_callback: None,
                    pacing: &pacing,
                    strict_tool_parsing: false,
                    parallel_tools,
                    max_tool_result_chars: 0,
                    keep_tool_context_turns: 2,
                    context_token_budget: 0,
                    receipt_generator: None,
                    knobs: &knobs,
                },
                history: &mut history,
                channel_name: "wss",
                channel_reply_target: None,
                cancellation_token: None,
                on_delta: None,
                shared_budget: None,
                channel: None,
                collected_receipts: None,
                event_tx: None,
                steering: None,
                new_messages_out: None,
                image_cache: None,
                memory: Some(crate::agent::memory_inject::TurnMemory {
                    handle: mem.as_ref(),
                    query: "*".into(),
                    sessions: vec![Some("A".into())],
                    suppress: true,
                    cfg: crate::agent::memory_inject::MemoryInjectConfig::default(),
                }),
                ingress: IngressContext::from_origin(origin),
                agent_alias: None,
                turn_id: &turn_id,
            });
            // Even a permissive parent cannot confer its authority on a child.
            zeroclaw_api::TOOL_LOOP_MEMORY_SESSIONS
                .scope(
                    vec!["A".into()],
                    zeroclaw_api::TOOL_LOOP_MEMORY_READ_POLICY
                        .scope(MemoryReadPolicy::SessionScoped, future),
                )
                .await
                .unwrap();
            let results = history
                .iter()
                .filter(|m| m.content.starts_with("[Tool results]"))
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(results.contains("memory_recall"), "{results}");
            assert!(results.contains("memory_export"), "{results}");
            assert!(
                !results.contains("PRIVATE_CHAT_CANARY"),
                "{origin:?}: {results}"
            );
            assert_eq!(
                results.contains("DURABLE_FACT"),
                origin != TurnOrigin::SubTurn,
                "{origin:?} parallel={parallel_tools}: {results}"
            );
        }
    }
}
