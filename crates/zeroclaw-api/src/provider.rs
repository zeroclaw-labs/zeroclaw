use crate::tool::ToolSpec;
use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use std::fmt::Write;
use std::sync::Arc;

pub const MAX_BUDGET_TOKENS: u32 = 128_000;

/// Parameters for native extended thinking support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeThinkingParams {
    pub budget_tokens: u32,
}

/// A single message in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
        }
    }

    pub fn tool(content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: content.into(),
        }
    }
}

/// A tool call requested by the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
    /// Provider-specific opaque extension fields that must round-trip
    /// unchanged on follow-up turns (e.g. Gemini 3 `thoughtSignature`
    /// carried as `extra_content.google.thought_signature`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_content: Option<serde_json::Value>,
}

/// Raw token counts from a single LLM API response.
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// Tokens served from the provider's prompt cache (Anthropic `cache_read_input_tokens`,
    /// OpenAI `prompt_tokens_details.cached_tokens`).
    pub cached_input_tokens: Option<u64>,
}

/// An LLM response that may contain text, tool calls, or both.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    /// Text content of the response (may be empty if only tool calls).
    pub text: Option<String>,
    /// Tool calls requested by the LLM.
    pub tool_calls: Vec<ToolCall>,
    /// Token usage reported by the provider, if available.
    pub usage: Option<TokenUsage>,
    /// Raw reasoning/thinking content from thinking models (e.g. DeepSeek-R1,
    /// Kimi K2.5, GLM-4.7). Preserved as an opaque pass-through so it can be
    /// sent back in subsequent API requests — some providers reject tool-call
    /// history that omits this field.
    pub reasoning_content: Option<String>,
}

impl ChatResponse {
    /// True when the LLM wants to invoke at least one tool.
    pub fn has_tool_calls(&self) -> bool {
        !self.tool_calls.is_empty()
    }

    /// Convenience: return text content or empty string.
    pub fn text_or_empty(&self) -> &str {
        self.text.as_deref().unwrap_or("")
    }
}

/// Request payload for provider chat calls.
#[derive(Debug, Clone, Copy)]
pub struct ChatRequest<'a> {
    pub messages: &'a [ChatMessage],
    pub tools: Option<&'a [ToolSpec]>,
    /// Native extended thinking parameters. When `Some`, providers that
    /// support extended thinking should send a dedicated thinking budget
    /// in the API request and force `temperature = 1.0`.
    pub thinking: Option<NativeThinkingParams>,
}

/// A tool result to feed back to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub content: String,
}

/// A message in a multi-turn conversation, including tool interactions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ConversationMessage {
    /// Regular chat message (system, user, assistant).
    Chat(ChatMessage),
    /// Tool calls from the assistant (stored for history fidelity).
    AssistantToolCalls {
        text: Option<String>,
        tool_calls: Vec<ToolCall>,
        /// Raw reasoning content from thinking models, preserved for round-trip
        /// fidelity with provider APIs that require it.
        reasoning_content: Option<String>,
    },
    /// Results of tool executions, fed back to the LLM.
    ToolResults(Vec<ToolResultMessage>),
}

/// A chunk of content from a streaming response.
#[derive(Debug, Clone)]
pub struct StreamChunk {
    /// Text delta for this chunk.
    pub delta: String,
    /// Reasoning/thinking delta (chain-of-thought from thinking models).
    pub reasoning: Option<String>,
    /// Whether this is the final chunk.
    pub is_final: bool,
    /// Approximate token count for this chunk (estimated).
    pub token_count: usize,
}

impl StreamChunk {
    /// Create a new non-final chunk.
    pub fn delta(text: impl Into<String>) -> Self {
        Self {
            delta: text.into(),
            reasoning: None,
            is_final: false,
            token_count: 0,
        }
    }

    /// Create a reasoning/thinking chunk.
    pub fn reasoning(text: impl Into<String>) -> Self {
        Self {
            delta: String::new(),
            reasoning: Some(text.into()),
            is_final: false,
            token_count: 0,
        }
    }

    /// Create a final chunk.
    pub fn final_chunk() -> Self {
        Self {
            delta: String::new(),
            reasoning: None,
            is_final: true,
            token_count: 0,
        }
    }

    /// Create an error chunk.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            delta: message.into(),
            reasoning: None,
            is_final: true,
            token_count: 0,
        }
    }

    /// Estimate tokens (rough approximation: ~4 chars per token).
    pub fn with_token_estimate(mut self) -> Self {
        self.token_count = self.delta.len().div_ceil(4);
        self
    }
}

/// Structured events emitted by provider streaming APIs.
///
/// This extends plain text chunk streaming with explicit tool-call signals so
/// agent loops can preserve native tool semantics without parsing payload text.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Text delta from the assistant.
    TextDelta(StreamChunk),
    /// Structured tool call emitted during streaming.
    ToolCall(ToolCall),
    /// A tool call that was already executed by the provider (e.g. Claude Code proxy).
    /// Emitted for observability only — not re-executed by the agent's dispatcher.
    PreExecutedToolCall { name: String, args: String },
    /// The result of a pre-executed tool call.
    PreExecutedToolResult { name: String, output: String },
    /// Token usage reported by the provider, typically just before [`StreamEvent::Final`].
    /// Providers that do not surface usage in streaming responses simply omit this event.
    Usage(TokenUsage),
    /// Stream has completed.
    Final,
}

impl StreamEvent {
    pub fn from_chunk(chunk: StreamChunk) -> Self {
        if chunk.is_final {
            Self::Final
        } else {
            Self::TextDelta(chunk)
        }
    }
}

/// Options for streaming chat requests.
#[derive(Debug, Clone, Copy, Default)]
pub struct StreamOptions {
    /// Whether to enable streaming (default: true).
    pub enabled: bool,
    /// Whether to include token counts in chunks.
    pub count_tokens: bool,
}

impl StreamOptions {
    /// Create new streaming options with enabled flag.
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            count_tokens: false,
        }
    }

    /// Enable token counting.
    pub fn with_token_count(mut self) -> Self {
        self.count_tokens = true;
        self
    }
}

/// Result type for streaming operations.
pub type StreamResult<T> = std::result::Result<T, StreamError>;

/// Errors that can occur during streaming.
#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("HTTP error: {0}")]
    Http(String),

    #[error("JSON parse error: {0}")]
    Json(serde_json::Error),

    #[error("Invalid SSE format: {0}")]
    InvalidSse(String),

    #[error("Provider error: {0}")]
    Provider(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Structured error returned when a requested capability is not supported.
#[derive(Debug, Clone, thiserror::Error)]
#[error("provider_capability_error provider={provider} capability={capability} message={message}")]
pub struct ProviderCapabilityError {
    pub provider: String,
    pub capability: String,
    pub message: String,
}

/// Provider capabilities declaration.
///
/// Describes what features a provider supports, enabling intelligent
/// adaptation of tool calling modes and request formatting.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderCapabilities {
    /// Whether the provider supports native tool calling via API primitives.
    pub native_tool_calling: bool,
    /// Whether the provider supports vision / image inputs.
    pub vision: bool,
    /// Whether the provider supports prompt caching.
    pub prompt_caching: bool,
    /// Whether the provider supports native extended thinking.
    pub extended_thinking: bool,
}

/// Provider-specific tool payload formats.
#[derive(Debug, Clone)]
pub enum ToolsPayload {
    /// Gemini API format (functionDeclarations).
    Gemini {
        function_declarations: Vec<serde_json::Value>,
    },
    /// Anthropic Messages API format (tools with input_schema).
    Anthropic { tools: Vec<serde_json::Value> },
    /// OpenAI Chat Completions API format (tools with function).
    OpenAI { tools: Vec<serde_json::Value> },
    /// Prompt-guided fallback (tools injected as text in system prompt).
    PromptGuided { instructions: String },
}

/// Industry-neutral sampling temperature. OpenAI, Gemini, OpenRouter, and
/// most OpenAI-compatible endpoints document 0.7 as their typical default;
/// Anthropic and Ollama override (1.0 and 0.0 respectively).
pub const BASELINE_TEMPERATURE: f64 = 0.7;

/// Output-token budget roomy enough for typical agent turns. Providers
/// override per family where the model's own context window is the
/// binding constraint.
pub const BASELINE_MAX_TOKENS: u32 = 4096;

/// HTTP timeout for cloud inference. Local providers (Ollama) override
/// upward since CPU/GPU-bound inference runs slower than round-tripping to
/// a hyperscaler.
pub const BASELINE_TIMEOUT_SECS: u64 = 120;

/// Wire protocol used when the provider doesn't declare one. Only OpenAI's
/// Codex stack uses the "responses" protocol; everything else speaks the
/// classic chat completions shape.
pub const BASELINE_WIRE_API: &str = "chat_completions";

#[async_trait]
pub trait Provider: Send + Sync {
    /// Query provider capabilities.
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    // ── Provider-family defaults ────────────────────────────────────────────
    // Called by every chat/stream method's `temperature.unwrap_or(self.default_temperature())`
    // and by `zeroclaw onboard` to prefill prompts with a visible default.
    // Baselines are the industry-neutral fallback; override per family where
    // the API docs disagree (Anthropic → 1.0, Ollama → 0.0 for deterministic
    // local inference).

    /// Temperature used when the caller passes `None`. Override per family.
    fn default_temperature(&self) -> f64 {
        BASELINE_TEMPERATURE
    }

    /// Max output tokens used when the caller / config doesn't set one.
    fn default_max_tokens(&self) -> u32 {
        BASELINE_MAX_TOKENS
    }

    /// HTTP timeout (seconds) used when the caller / config doesn't set one.
    fn default_timeout_secs(&self) -> u64 {
        BASELINE_TIMEOUT_SECS
    }

    /// Canonical public API endpoint, when there is one. Returned as a
    /// string slice so provider impls can serve from `const &'static str`s
    /// without allocations. `None` = provider has no universal endpoint
    /// (local providers, auth-less CLIs, user-BYO endpoints).
    fn default_base_url(&self) -> Option<&str> {
        None
    }

    /// Wire protocol variant. Either `"responses"` (OpenAI Codex-style) or
    /// `"chat_completions"` (everything else). Providers override to their
    /// native format.
    fn default_wire_api(&self) -> &str {
        BASELINE_WIRE_API
    }

    /// Convert tool specifications to provider-native format.
    fn convert_tools(&self, tools: &[ToolSpec]) -> ToolsPayload {
        ToolsPayload::PromptGuided {
            instructions: build_tool_instructions_text(tools),
        }
    }

    /// Simple one-shot chat (single user message, no explicit system prompt).
    ///
    /// `temperature == None` means "use `self.default_temperature()`". The
    /// unwrap lives inside each provider impl, not at every call site.
    async fn simple_chat(
        &self,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        self.chat_with_system(None, message, model, temperature)
            .await
    }

    /// One-shot chat with optional system prompt. See `simple_chat` for
    /// the `temperature` contract.
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String>;

    /// Fetch the list of available model IDs for this provider.
    ///
    /// Used by onboard to present a live model picker. Default bails with
    /// "not supported"; concrete providers override to hit their own public
    /// endpoint (OpenRouter, Ollama) or delegate to the shared models.dev
    /// catalog (no auth required) in `zeroclaw_providers::models_dev`.
    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        anyhow::bail!("live model listing is not supported for this provider")
    }

    /// Multi-turn conversation. See `simple_chat` for the `temperature`
    /// contract.
    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let system = messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str());
        let last_user = messages
            .iter()
            .rfind(|m| m.role == "user")
            .map(|m| m.content.as_str())
            .unwrap_or("");
        self.chat_with_system(system, last_user, model, temperature)
            .await
    }

    /// Structured chat API for agent loop callers. See `simple_chat` for
    /// the `temperature` contract.
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        if let Some(tools) = request.tools
            && !tools.is_empty()
            && !self.supports_native_tools()
        {
            let tool_instructions = match self.convert_tools(tools) {
                ToolsPayload::PromptGuided { instructions } => instructions,
                payload => {
                    anyhow::bail!(
                        "Provider returned non-prompt-guided tools payload ({payload:?}) while supports_native_tools() is false"
                    )
                }
            };
            let mut modified_messages = request.messages.to_vec();

            if let Some(system_message) = modified_messages.iter_mut().find(|m| m.role == "system")
            {
                if !system_message.content.is_empty() {
                    system_message.content.push_str("\n\n");
                }
                system_message.content.push_str(&tool_instructions);
            } else {
                modified_messages.insert(0, ChatMessage::system(tool_instructions));
            }

            let text = self
                .chat_with_history(&modified_messages, model, temperature)
                .await?;
            return Ok(ChatResponse {
                text: Some(text),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            });
        }

        let text = self
            .chat_with_history(request.messages, model, temperature)
            .await?;
        Ok(ChatResponse {
            text: Some(text),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        })
    }

    /// Whether provider supports native tool calls over API.
    fn supports_native_tools(&self) -> bool {
        self.capabilities().native_tool_calling
    }

    /// Whether provider supports multimodal vision input.
    fn supports_vision(&self) -> bool {
        self.capabilities().vision
    }

    /// Warm up the HTTP connection pool.
    async fn warmup(&self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Chat with tool definitions for native function calling support.
    /// See `simple_chat` for the `temperature` contract.
    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        _tools: &[serde_json::Value],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        let text = self.chat_with_history(messages, model, temperature).await?;
        Ok(ChatResponse {
            text: Some(text),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        })
    }

    /// Whether provider supports streaming responses.
    fn supports_streaming(&self) -> bool {
        false
    }

    /// Whether provider can emit structured tool-call stream events.
    fn supports_streaming_tool_events(&self) -> bool {
        false
    }

    /// Streaming chat with optional system prompt. See `simple_chat` for
    /// the `temperature` contract.
    fn stream_chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
        _options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
        stream::empty().boxed()
    }

    /// Streaming chat with history. See `simple_chat` for the `temperature`
    /// contract.
    fn stream_chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
        let system = messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str());
        let last_user = messages
            .iter()
            .rfind(|m| m.role == "user")
            .map(|m| m.content.as_str())
            .unwrap_or("");
        self.stream_chat_with_system(system, last_user, model, temperature, options)
    }

    /// Structured streaming chat interface. See `simple_chat` for the
    /// `temperature` contract.
    fn stream_chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
        self.stream_chat_with_history(request.messages, model, temperature, options)
            .map(|chunk_result| chunk_result.map(StreamEvent::from_chunk))
            .boxed()
    }
}

/// Blanket implementation: `Arc<T>` delegates all `Provider` methods to `T`.
///
/// This eliminates the need for manual `impl Provider for Arc<MyProvider>`
/// boilerplate in test and production code.
#[async_trait]
impl<T: Provider + ?Sized> Provider for Arc<T> {
    fn capabilities(&self) -> ProviderCapabilities {
        self.as_ref().capabilities()
    }

    fn default_temperature(&self) -> f64 {
        self.as_ref().default_temperature()
    }

    fn default_max_tokens(&self) -> u32 {
        self.as_ref().default_max_tokens()
    }

    fn default_timeout_secs(&self) -> u64 {
        self.as_ref().default_timeout_secs()
    }

    fn default_base_url(&self) -> Option<&str> {
        self.as_ref().default_base_url()
    }

    fn default_wire_api(&self) -> &str {
        self.as_ref().default_wire_api()
    }

    fn convert_tools(&self, tools: &[ToolSpec]) -> ToolsPayload {
        self.as_ref().convert_tools(tools)
    }

    fn supports_native_tools(&self) -> bool {
        self.as_ref().supports_native_tools()
    }

    fn supports_vision(&self) -> bool {
        self.as_ref().supports_vision()
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        self.as_ref()
            .chat_with_system(system_prompt, message, model, temperature)
            .await
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        self.as_ref()
            .chat_with_history(messages, model, temperature)
            .await
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        self.as_ref().chat(request, model, temperature).await
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        self.as_ref().warmup().await
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        self.as_ref()
            .chat_with_tools(messages, tools, model, temperature)
            .await
    }

    fn supports_streaming(&self) -> bool {
        self.as_ref().supports_streaming()
    }

    fn supports_streaming_tool_events(&self) -> bool {
        self.as_ref().supports_streaming_tool_events()
    }

    fn stream_chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
        self.as_ref()
            .stream_chat_with_system(system_prompt, message, model, temperature, options)
    }

    fn stream_chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
        self.as_ref()
            .stream_chat_with_history(messages, model, temperature, options)
    }

    fn stream_chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
        self.as_ref()
            .stream_chat(request, model, temperature, options)
    }
}

/// Build tool instructions text for prompt-guided tool calling.
pub fn build_tool_instructions_text(tools: &[ToolSpec]) -> String {
    let mut instructions = String::new();

    instructions.push_str("## Tool Use Protocol\n\n");
    instructions.push_str("To use a tool, wrap a JSON object in <tool_call></tool_call> tags:\n\n");
    instructions.push_str("<tool_call>\n");
    instructions.push_str(r#"{"name": "tool_name", "arguments": {"param": "value"}}"#);
    instructions.push_str("\n</tool_call>\n\n");
    instructions.push_str("You may use multiple tool calls in a single response. ");
    instructions.push_str("After tool execution, results appear in <tool_result> tags. ");
    instructions
        .push_str("Continue reasoning with the results until you can give a final answer.\n\n");
    instructions.push_str("### Available Tools\n\n");

    for tool in tools {
        writeln!(&mut instructions, "**{}**: {}", tool.name, tool.description)
            .expect("writing to String cannot fail");

        let parameters =
            serde_json::to_string(&tool.parameters).unwrap_or_else(|_| "{}".to_string());
        writeln!(&mut instructions, "Parameters: `{parameters}`")
            .expect("writing to String cannot fail");
        instructions.push('\n');
    }

    instructions
}
