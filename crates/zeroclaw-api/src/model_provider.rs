use crate::tool::ToolSpec;
use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use std::fmt::Write;
use std::sync::Arc;

pub const MAX_BUDGET_TOKENS: u32 = 128_000;
/// Anthropic's documented minimum for extended-thinking `budget_tokens`.
/// Requests below this are rejected with 400 by the provider; clamping at
/// resolution time gives a clearer error site than the first API call.
pub const MIN_BUDGET_TOKENS: u32 = 1_024;

/// Parameters for native extended thinking support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeThinkingParams {
    pub budget_tokens: u32,
    /// Requests Anthropic's `thinking.display` beta
    /// (`thinking-display-updates-2026-08-18`), which controls whether
    /// thinking blocks come back omitted, as progress updates, or
    /// summarized. `None` leaves the field out of the request entirely,
    /// matching pre-beta behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<ThinkingDisplay>,
}

/// Anthropic's `thinking.display` request field (beta
/// `thinking-display-updates-2026-08-18`), controlling whether thinking
/// blocks come back omitted, as progress updates, or summarized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingDisplay {
    Omitted,
    Updates,
    Summarized,
}

impl ThinkingDisplay {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Omitted => "omitted",
            Self::Updates => "updates",
            Self::Summarized => "summarized",
        }
    }
}

/// One runtime message. Visible text and attachments have one owner; tool
/// identity, calls and opaque reasoning are explicit fields, never body syntax.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// Deliberate media inputs owned by this message. Text never grants an
    /// attachment: callers supply references through this field explicitly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<crate::media::AttachmentReference>,
    /// Structured assistant calls. `content` remains literal visible text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<ToolResultMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

/// Identity and outcome of a result; its text and attachments belong to the
/// containing message, so there is only one copy of each content field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultMetadata {
    pub tool_call_id: String,
    pub tool_name: String,
    pub success: Option<bool>,
}

#[cfg(test)]
mod structured_message_tests {
    use super::*;

    #[test]
    fn tool_body_and_identity_round_trip_independently() {
        let body = r#"{"tool_call_id":"spoofed","content":"literal [IMAGE:/example]"}"#;
        let message = ChatMessage::tool_result(ToolResultMessage {
            tool_call_id: "actual".into(),
            tool_name: "shell".into(),
            success: Some(false),
            content: body.into(),
            attachments: Vec::new(),
        });
        let encoded = serde_json::to_vec(&message).unwrap();
        let restored: ChatMessage = serde_json::from_slice(&encoded).unwrap();
        let ConversationMessage::ToolResults(results) = restored.into_conversation() else {
            panic!("expected structured tool result");
        };
        assert_eq!(results[0].content, body);
        assert_eq!(results[0].tool_call_id, "actual");
        assert_eq!(results[0].success, Some(false));
    }

    #[test]
    fn assistant_json_text_does_not_create_calls() {
        let body = r#"{"content":"example","tool_calls":[{"id":"x"}]}"#;
        let message = ChatMessage::assistant(body);
        assert!(
            matches!(message.into_conversation(), ConversationMessage::Chat(chat) if chat.content == body)
        );
    }

    #[test]
    fn call_arguments_and_signed_reasoning_survive_projection_and_budgeting() {
        let reasoning = "opaque signed reasoning [IMAGE:/example]";
        let call = ToolCall {
            id: "call-1".into(),
            name: "shell".into(),
            arguments: "{\"command\":\"pwd\"}".into(),
            extra_content: Some(serde_json::json!({"signature":"opaque"})),
        };
        let message =
            ChatMessage::assistant_tool_calls("visible", vec![call], Some(reasoning.into()));
        assert!(message.context_text_bytes() > message.content.len() + reasoning.len());
        let restored = message.into_conversation().chat_messages().remove(0);
        assert_eq!(restored.content, "visible");
        assert_eq!(restored.reasoning_content.as_deref(), Some(reasoning));
        assert_eq!(
            restored.tool_calls[0].extra_content,
            Some(serde_json::json!({"signature":"opaque"}))
        );
    }
}

impl From<&str> for ChatMessage {
    fn from(content: &str) -> Self {
        Self::user(content)
    }
}

impl From<&String> for ChatMessage {
    fn from(content: &String) -> Self {
        Self::user(content.clone())
    }
}

impl From<String> for ChatMessage {
    fn from(content: String) -> Self {
        Self::user(content)
    }
}

impl From<&ChatMessage> for ChatMessage {
    fn from(message: &ChatMessage) -> Self {
        message.clone()
    }
}

pub const PRUNED_TOOL_EXCHANGE_SUMMARY_PREFIX: &str = "[Tool exchange:";
pub const PRUNED_TOOL_EXCHANGE_SUMMARY_SUFFIX: &str = "results collapsed]";
pub const PRUNED_CONTEXT_SEPARATOR: &str = "[context continues]";

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
            reasoning_content: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
            reasoning_content: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
            reasoning_content: None,
        }
    }

    pub fn assistant_tool_calls(
        text: impl Into<String>,
        tool_calls: Vec<ToolCall>,
        reasoning_content: Option<String>,
    ) -> Self {
        Self {
            tool_calls,
            reasoning_content,
            ..Self::assistant(text)
        }
    }

    pub fn tool_result(result: ToolResultMessage) -> Self {
        Self {
            role: "tool".into(),
            content: result.content,
            tool_result: Some(ToolResultMetadata {
                tool_call_id: result.tool_call_id,
                tool_name: result.tool_name,
                success: result.success,
            }),
            attachments: result.attachments,
            tool_calls: Vec::new(),
            reasoning_content: None,
        }
    }

    pub fn into_conversation(self) -> ConversationMessage {
        if self.role == "tool"
            && let Some(result) = self.tool_result
        {
            ConversationMessage::ToolResults(vec![ToolResultMessage {
                tool_call_id: result.tool_call_id,
                tool_name: result.tool_name,
                success: result.success,
                content: self.content,
                attachments: self.attachments,
            }])
        } else if self.role == "assistant"
            && self.attachments.is_empty()
            && (!self.tool_calls.is_empty() || self.reasoning_content.is_some())
        {
            ConversationMessage::AssistantToolCalls {
                text: (!self.content.is_empty()).then_some(self.content),
                tool_calls: self.tool_calls,
                reasoning_content: self.reasoning_content,
            }
        } else {
            ConversationMessage::Chat(self)
        }
    }

    pub fn append_note(&mut self, note: &str) {
        if !self.content.is_empty() {
            self.content.push_str("\n\n");
        }
        self.content.push_str(note);
    }

    /// Text population used by context budgeting, including typed tool fields.
    pub fn context_text_bytes(&self) -> usize {
        self.content.len()
            + self.reasoning_content.as_ref().map_or(0, String::len)
            + self
                .tool_calls
                .iter()
                .map(|call| {
                    call.id.len()
                        + call.name.len()
                        + call.arguments.len()
                        + call
                            .extra_content
                            .as_ref()
                            .map_or(0, |value| value.to_string().len())
                })
                .sum::<usize>()
            + self.tool_result.as_ref().map_or(0, |result| {
                result.tool_call_id.len() + result.tool_name.len()
            })
    }

    pub fn with_attachments(mut self, attachments: Vec<crate::media::AttachmentReference>) -> Self {
        self.attachments = attachments;
        self
    }

    pub fn pruned_tool_exchange_summary(tool_count: usize) -> String {
        format!(
            "{PRUNED_TOOL_EXCHANGE_SUMMARY_PREFIX} {tool_count} tool call(s) — {PRUNED_TOOL_EXCHANGE_SUMMARY_SUFFIX}"
        )
    }

    pub fn pruned_context_separator() -> Self {
        Self::user(PRUNED_CONTEXT_SEPARATOR)
    }

    pub fn is_pruned_tool_exchange_summary(&self) -> bool {
        self.role == "assistant"
            && self
                .content
                .starts_with(PRUNED_TOOL_EXCHANGE_SUMMARY_PREFIX)
            && self.content.contains(PRUNED_TOOL_EXCHANGE_SUMMARY_SUFFIX)
    }

    pub fn is_pruned_context_separator(&self) -> bool {
        self.role == "user" && self.content.trim() == PRUNED_CONTEXT_SEPARATOR
    }

    pub fn should_skip_internal_pruning_marker(messages: &[Self], index: usize) -> bool {
        let Some(msg) = messages.get(index) else {
            return false;
        };
        if msg.is_pruned_tool_exchange_summary() {
            return true;
        }
        msg.is_pruned_context_separator()
            && index
                .checked_sub(1)
                .and_then(|previous| messages.get(previous))
                .is_some_and(Self::is_pruned_tool_exchange_summary)
    }

    pub fn is_system(&self) -> bool {
        self.role == "system"
    }

    pub fn is_user(&self) -> bool {
        self.role == "user"
    }

    pub fn sanitize_leading_turn_order(messages: &mut Vec<Self>) {
        let first_non_system = messages
            .iter()
            .position(|m| !m.is_system())
            .unwrap_or(messages.len());
        let mut drop_to = first_non_system;
        while drop_to < messages.len() && !messages[drop_to].is_user() {
            drop_to += 1;
        }
        if drop_to > first_non_system {
            messages.drain(first_non_system..drop_to);
        }
    }
}

/// A tool call requested by the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
    /// ModelProvider-specific opaque extension fields that must round-trip
    /// unchanged on follow-up turns (e.g. Gemini 3 `thoughtSignature`
    /// carried as `extra_content.google.thought_signature`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_content: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    /// Total prompt size: uncached + cached input tokens (including the
    /// cache-write subset when the provider reports it separately).
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// Subset of `input_tokens` that was served from the model_provider's
    /// prompt cache (Anthropic `cache_read_input_tokens`,
    /// OpenAI `prompt_tokens_details.cached_tokens`).
    pub cached_input_tokens: Option<u64>,
    /// Subset of `input_tokens` that the model_provider wrote into its
    /// prompt cache on this request (Anthropic
    /// `cache_creation_input_tokens`, OpenAI-compatible
    /// `prompt_tokens_details.cache_creation_input_tokens`). Providers
    /// bill these at a premium over the plain input rate.
    pub cache_creation_input_tokens: Option<u64>,
}

/// An LLM response that may contain text, tool calls, or both.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    /// Original ordered, adapter-classified protocol content. This creates
    /// the order/media fact absent from the legacy text/tool projections.
    /// Empty means the adapter only supplied the legacy projections.
    pub output_blocks: Vec<ProviderOutputBlock>,
    /// Text content of the response (may be empty if only tool calls).
    pub text: Option<String>,
    /// Tool calls requested by the LLM.
    pub tool_calls: Vec<ToolCall>,
    /// Token usage reported by the model_provider, if available.
    pub usage: Option<TokenUsage>,
    /// Raw reasoning/thinking content from thinking models (e.g. DeepSeek-R1,
    /// Kimi K2.5, GLM-4.7). Preserved as an opaque pass-through so it can be
    /// sent back in subsequent API requests — some model_providers reject tool-call
    /// history that omits this field.
    pub reasoning_content: Option<String>,
}

/// Adapter-classified output content, excluding HTTP/authentication metadata.
/// These blocks preserve bytes and references without fetching remote media.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderOutputBlock {
    /// Adapter-observed protocol evidence; this is not conversation content.
    /// `provider_item` and `content_metadata` retain supplemental fields with
    /// an output/content position or call identity. Normalized bodies have
    /// their own blocks and are never copied into that evidence.
    ProtocolMetadata {
        metadata: serde_json::Value,
    },
    Text {
        text: String,
    },
    Reasoning {
        representation: String,
        bytes: Vec<u8>,
        replay_requirement: String,
    },
    ToolCall {
        call: ToolCall,
    },
    Media {
        media_type: String,
        format: Option<String>,
        source: ProviderMediaSource,
    },
    /// Content fields classified by the adapter, never a full HTTP object.
    Other {
        type_name: String,
        content: serde_json::Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderMediaSource {
    Bytes {
        bytes: Vec<u8>,
    },
    Reference {
        locator: String,
        expires_at: Option<i64>,
    },
}

/// A transport-successful provider result that cannot complete a request.
///
/// The result has neither user-visible final text nor native tool calls.
/// Reasoning is intentionally not part of this contract because it is opaque
/// provider round-trip metadata rather than a final answer.
#[derive(Debug)]
pub struct SemanticEmptyTerminalCompletion;

impl std::fmt::Display for SemanticEmptyTerminalCompletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("provider completed without final text or tool calls")
    }
}

impl std::error::Error for SemanticEmptyTerminalCompletion {}

impl ChatResponse {
    /// True when the LLM wants to invoke at least one tool.
    pub fn has_tool_calls(&self) -> bool {
        !self.tool_calls.is_empty()
    }

    /// Convenience: return text content or empty string.
    pub fn text_or_empty(&self) -> &str {
        self.text.as_deref().unwrap_or("")
    }

    /// True when this response cannot make progress or complete a turn.
    ///
    /// Reasoning content is intentionally excluded: it may need to be
    /// round-tripped to a provider, but it is not a user-visible final answer.
    /// A response containing one or more tool calls remains valid even when
    /// its text is empty.
    pub fn is_semantically_empty_terminal(&self) -> bool {
        strip_think_tags(self.text_or_empty()).is_empty()
            && self.tool_calls.is_empty()
            && !self.output_blocks.iter().any(|block| match block {
                ProviderOutputBlock::Media { .. } | ProviderOutputBlock::ToolCall { .. } => true,
                ProviderOutputBlock::Text { text } => !strip_think_tags(text).is_empty(),
                _ => false,
            })
    }
}

/// Remove inline `<think>...</think>` reasoning before terminal-response
/// classification or user-visible parsing.
///
/// An unclosed opening tag suppresses the remainder so partial reasoning never
/// becomes final output.
pub fn strip_think_tags(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;
    loop {
        if let Some(start) = remaining.find("<think>") {
            result.push_str(&remaining[..start]);
            if let Some(end) = remaining[start..].find("</think>") {
                remaining = &remaining[start + end + "</think>".len()..];
            } else {
                break;
            }
        } else {
            result.push_str(remaining);
            break;
        }
    }
    result.trim().to_string()
}

/// Requested tool-call cadence for one provider response.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRoundPolicy {
    #[default]
    Batch,
    Single,
}

/// Request payload for model_provider chat calls.
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
    /// Observed execution outcome; absent when legacy history did not record it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<crate::media::AttachmentReference>,
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
        /// fidelity with model_provider APIs that require it.
        reasoning_content: Option<String>,
    },
    /// Results of tool executions, fed back to the LLM.
    ToolResults(Vec<ToolResultMessage>),
}

impl ConversationMessage {
    /// Runtime projection. Content stays literal; calls/results remain typed.
    pub fn chat_messages(&self) -> Vec<ChatMessage> {
        match self {
            Self::Chat(chat) => vec![chat.clone()],
            Self::AssistantToolCalls {
                text,
                tool_calls,
                reasoning_content,
            } => vec![ChatMessage::assistant_tool_calls(
                text.clone().unwrap_or_default(),
                tool_calls.clone(),
                reasoning_content.clone(),
            )],
            Self::ToolResults(results) => results
                .iter()
                .cloned()
                .map(ChatMessage::tool_result)
                .collect(),
        }
    }
}

/// Project the visible durable turns to runtime messages. Typed tool-loop
/// fields are retained on each returned message; system prompts are excluded.
///
/// Callers that own the agent's authoritative post-turn history (after
/// budget-enforcement trimming) should use this to replace a durable
/// transcript wholesale rather than appending the turn's delta on top of a
/// transcript the agent may have already trimmed underneath it.
pub fn durable_chat_messages(history: &[ConversationMessage]) -> Vec<ChatMessage> {
    history
        .iter()
        .filter_map(|message| match message {
            ConversationMessage::Chat(chat) if chat.role != "system" => Some(chat.clone()),
            _ => None,
        })
        .collect()
}

/// Count the conversation entries a transcript projects into, without
/// materializing them. One entry per chat message; an assistant tool-call
/// message contributes its non-empty text plus one entry per call; a tool
/// result folds into the entry of a call still awaiting a result (FIFO by
/// `tool_call_id`) and otherwise counts as its own orphan entry.
///
/// The ACP session store maintains a persisted per-session counter with the
/// same semantics. Runtime projection (`conversation_message_entries`) and
/// this function are two expressions of one counting rule and must agree on
/// every input.
pub fn projected_entry_count(messages: &[ConversationMessage]) -> usize {
    let mut count = 0usize;
    let mut open_calls = std::collections::HashMap::<&str, usize>::new();
    for message in messages {
        match message {
            ConversationMessage::Chat(_) => count += 1,
            ConversationMessage::AssistantToolCalls {
                text, tool_calls, ..
            } => {
                if text.as_deref().is_some_and(|text| !text.is_empty()) {
                    count += 1;
                }
                count += tool_calls.len();
                for call in tool_calls {
                    *open_calls.entry(call.id.as_str()).or_insert(0) += 1;
                }
            }
            ConversationMessage::ToolResults(results) => {
                for result in results {
                    match open_calls.get_mut(result.tool_call_id.as_str()) {
                        Some(open) if *open > 0 => *open -= 1,
                        _ => count += 1,
                    }
                }
            }
        }
    }
    count
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

/// Structured events emitted by model_provider streaming APIs.
/// This extends plain text chunk streaming with explicit tool-call signals so
/// agent loops can preserve native tool semantics without parsing payload text.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Protocol position of the immediately following semantic segment.
    /// Offsets refer to the original content observed by capture.
    ContentPosition(ProviderContentPosition),
    /// A complete non-text/opaque content block. Text and tool
    /// deltas retain their existing streaming variants; this is never flattened.
    OutputBlock(ProviderOutputBlock),
    /// Text delta from the assistant.
    TextDelta(StreamChunk),
    /// Transient, human-readable thinking progress. Surfaced to the user
    /// (gated by the runtime visibility policy) and never persisted into
    /// reasoning_content.
    ThinkingDelta(String),
    /// Durable, replay-only finalized reasoning payload (signed thinking
    /// blocks in the provider's history-replay representation). Appended to
    /// `ChatResponse::reasoning_content` for the next provider request and
    /// never surfaced as user-visible progress.
    ReasoningFinalized(String),
    /// Structured tool call emitted during streaming.
    ToolCall(ToolCall),
    /// A tool call that was already executed by the model_provider (e.g. Claude Code proxy).
    /// Emitted for observability only — not re-executed by the agent's dispatcher.
    PreExecutedToolCall {
        /// Actual provider identity; absence must not be replaced by a name.
        provider_call_id: Option<String>,
        name: String,
        args: String,
    },
    /// The result of a pre-executed tool call.
    PreExecutedToolResult {
        provider_call_id: Option<String>,
        name: String,
        output: String,
    },
    /// Token usage reported by the provider, typically just before [`StreamEvent::Final`].
    /// Providers that do not surface usage in streaming responses simply omit this event.
    Usage(TokenUsage),
    /// Adapter-classified protocol evidence, separate from message content.
    /// Authentication headers and credentials must never enter this value.
    ProtocolMetadata(serde_json::Value),
    /// Stream has completed.
    Final,
}

/// Native protocol coordinates, distinct from final response array indices.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProviderContentPosition {
    pub output_index: u64,
    pub content_index: Option<u64>,
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

/// A provider safety refusal that completed at the transport layer but cannot
/// be accepted as an assistant response.
///
/// The optional usage belongs to the refusing attempt. It is carried on the
/// typed cause so reliability and turn accounting can bill that work without
/// treating it as accepted-response context usage. `category` is diagnostic
/// metadata only and must not be rendered to users.
#[derive(Debug, Clone, thiserror::Error)]
#[error("anthropic refusal: model declined this request (safety classifiers)")]
pub struct ModelRefusalError {
    /// Model requested on the refusing attempt.
    pub requested_model: String,
    /// Refusal category token, when the provider supplied one.
    pub category: Option<String>,
    /// Normalized usage billed by the refusing attempt.
    pub usage: Option<Box<TokenUsage>>,
    /// Exact reliability candidate that emitted a streamed refusal.
    ///
    /// Leaf providers leave this unset. Composite providers fill it while
    /// forwarding a stream so a non-streaming recovery can skip exactly the
    /// already-billed candidate.
    pub attempted_candidate: Option<String>,
    /// Position of that candidate in the active reliability domain.
    ///
    /// This disambiguates same-profile fallback models, which intentionally
    /// share one configured candidate/cooldown identity.
    pub attempted_candidate_index: Option<usize>,
}

/// Errors that can occur during streaming.
#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("HTTP error: {0}")]
    Http(String),

    /// The connection for the failing request hop could not be opened
    /// (connect, TLS handshake or DNS), as reported by the transport at
    /// the send site. For a request that followed no redirect, nothing
    /// was delivered. A redirect-following client may already have
    /// delivered an earlier hop; callers that must not re-send delivered
    /// work cannot rely on this variant alone.
    ///
    /// The display text matches [`StreamError::Http`] so logs, diagnostics
    /// and user-facing messages are unchanged.
    #[error("HTTP error: {0}")]
    ConnectFailed(String),

    /// The transport observed a response/read deadline after dispatch.
    #[error("HTTP error: {0}")]
    ResponseTimeout(String),

    /// EOF arrived without the adapter's required terminal protocol signal.
    #[error("SSE stream closed before {0}: response truncated")]
    MissingFinal(String),

    #[error("JSON parse error: {0}")]
    Json(serde_json::Error),

    #[error("Invalid SSE format: {0}")]
    InvalidSse(String),

    #[error("ModelProvider error: {0}")]
    ModelProvider(String),

    /// An explicit provider error frame. Preserve the classified cause and
    /// codes while leaving retry policy with the dispatch owner.
    #[error("ModelProvider error: {0}")]
    ProtocolFailure(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),

    /// A provider returned a non-success HTTP response before streaming began.
    #[error("ModelProvider HTTP {status}: {message}")]
    HttpStatus { status: u16, message: String },

    #[error(transparent)]
    ModelRefusal(#[from] Box<ModelRefusalError>),

    /// The provider already exhausted its own retry/fallback budget producing
    /// this error; consumers must not retry or fall back. Produced only when a
    /// completed non-streaming call is synthesized into a stream (the wrapped
    /// failure already survived the full ladder). A genuine streaming leg never
    /// emits it, so fallback recovery for streamed failures is unaffected.
    /// The payload is the completed call's failure; consumers walk its chain
    /// for the typed terminal cause beneath it.
    #[error("terminal provider error: {0}")]
    Terminal(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),

    /// Durable observation failed or the physical attempt became uncertain.
    /// This is an execution fence, not exhausted provider retry policy.
    #[error("session capture stopped: {0}")]
    CaptureStopped(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Structured error returned when a requested capability is not supported.
#[derive(Debug, Clone, thiserror::Error)]
#[error(
    "provider_capability_error model_provider={model_provider} capability={capability} message={message}"
)]
pub struct ProviderCapabilityError {
    pub model_provider: String,
    pub capability: String,
    pub message: String,
}

/// Typed marker returned when a provider intentionally has no live model-list
/// endpoint. Callers may use a separate canonical static catalog only for this
/// condition; transport, authentication, and malformed-response failures must
/// remain actionable.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("live model listing is not supported for this model_provider")]
pub struct ModelListingUnsupportedError;

/// ModelProvider capabilities declaration.
/// Describes what features a model_provider supports, enabling intelligent
/// adaptation of tool calling modes and request formatting.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderCapabilities {
    /// Whether the model_provider supports native tool calling via API primitives.
    pub native_tool_calling: bool,
    /// Whether the model_provider supports vision / image inputs.
    pub vision: bool,
    /// Whether the model_provider supports prompt caching.
    pub prompt_caching: bool,
    /// Whether the provider supports native extended thinking.
    pub extended_thinking: bool,
}

/// ModelProvider-specific tool payload formats.
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

/// HTTP timeout for cloud inference. Local model_providers (Ollama) override
/// upward since CPU/GPU-bound inference runs slower than round-tripping to
/// a hyperscaler.
pub const BASELINE_TIMEOUT_SECS: u64 = 120;

/// Wire protocol used when the model_provider doesn't declare one. Only OpenAI's
/// Codex stack uses the "responses" protocol; everything else speaks the
/// classic chat completions shape.
pub const BASELINE_WIRE_API: &str = "chat_completions";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelPricing {
    /// Input/prompt tokens per-token rate (USD per token, e.g. `"0.000005"` = $5/1M tokens).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Output/completion tokens per-token rate (USD per token, e.g. `"0.000020"` = $20/1M tokens).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<String>,
    /// Cached input read rate — per-token charge for reading cached prompt data
    /// (USD per token, e.g. `"0.000001"` = $1/1M tokens). Kilo Gateway specific.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cache_read: Option<String>,
    /// Cached input write rate — per-token charge for writing prompt data to cache
    /// (USD per token, e.g. `"0.000001"` = $1/1M tokens). Kilo Gateway specific.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cache_write: Option<String>,
}

/// Model info with optional pricing — returned by `list_models_with_pricing`.
#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelPricing>,
    /// Maximum input window in tokens, as reported by the provider catalog.
    /// `None` when the catalog does not publish one — callers must treat that
    /// as "unknown" rather than substituting a default, so an operator can be
    /// told the window is unset instead of silently getting a stub value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
}

#[async_trait]
pub trait ModelProvider: Send + Sync + crate::attribution::Attributable {
    /// Configured dispatch identity. A decorator's logging attribution may
    /// describe its inner provider; its requested route remains its own alias.
    fn dispatch_reference(&self) -> &str {
        self.alias()
    }

    /// A composition node dispatches its physical work through child
    /// ProviderDispatch boundaries. Capture admission belongs to those leaves.
    fn is_dispatch_composite(&self) -> bool {
        false
    }

    /// Whether repeated requests for `model` are dispatched to one stable
    /// provider/model identity.
    ///
    /// The default is deliberately unstable. Known leaf providers are marked
    /// stable at their construction choke point; composite and out-of-tree
    /// providers must opt in only when they can prove one concrete dispatch
    /// identity. Callers use this fact to fail closed for identity-sensitive
    /// behavior such as persistent full-response caching.
    fn has_stable_request_identity(&self, _model: &str) -> bool {
        false
    }

    /// Whether this exact request shape can be replayed for `model` on the same
    /// provider/model route without internal retries, fallback, or key
    /// mutation. Runtime fails closed when this capability is absent.
    #[doc(hidden)]
    fn supports_exact_request_replay(&self, _request: ChatRequest<'_>, _model: &str) -> bool {
        false
    }

    /// Query model_provider capabilities.
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    /// Query the effective capabilities for the model that will be dispatched.
    ///
    /// Most providers have one capability set for every model and inherit this
    /// default. Composite providers override it when the model selects a route
    /// or when failover can reach children with different capabilities.
    fn capabilities_for_model(&self, _model: &str) -> ProviderCapabilities {
        let mut capabilities = self.capabilities();
        // Preserve compatibility with providers that historically overrode the
        // convenience accessors instead of capabilities(). Composite overrides
        // should still make the model-aware value authoritative.
        capabilities.native_tool_calling = self.supports_native_tools();
        capabilities.vision = self.supports_vision();
        capabilities
    }

    /// Whether every reachable route for this model honors a native one-call control.
    /// Prompt guidance and provider-executed tools do not establish support.
    fn supports_single_tool_rounds(&self, _model: &str) -> bool {
        false
    }

    /// Explicit per-request policy. Unsupported adapters retain their batch path.
    async fn chat_with_tool_round_policy(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
        _policy: ToolRoundPolicy,
    ) -> anyhow::Result<ChatResponse> {
        self.chat(request, model, temperature).await
    }

    fn stream_chat_with_tool_round_policy(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
        _policy: ToolRoundPolicy,
    ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
        self.stream_chat(request, model, temperature, options)
    }

    /// Name the entry that forced `vision` to `false` on this provider's
    /// [`Self::capabilities_for_model`], for providers that aggregate several
    /// named entries (e.g. a primary plus configured fallbacks) into one
    /// capability set. Returns `None` when this provider is not such an
    /// aggregate, or when nothing about it limits vision for `model`.
    fn vision_limited_by(&self, _model: &str) -> Option<String> {
        None
    }

    /// Whether the selected request can reach both native-tool and text-only
    /// candidates.
    ///
    /// Ordinary providers are homogeneous and inherit `false`. Composite
    /// providers override this so callers that must select one tool protocol
    /// before dispatch can reject an incompatible strict configuration.
    fn has_mixed_native_tool_support_for_model(&self, _model: &str) -> bool {
        false
    }

    /// Family-preferred temperature default. Override per family. Documented
    /// for introspection only; never use to convert `None` into a wire value.
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
    /// string slice so model_provider impls can serve from `const &'static str`s
    /// without allocations. `None` = model_provider has no universal endpoint
    /// (local model_providers, auth-less CLIs, user-BYO endpoints).
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
    /// `temperature == None` means the field is omitted on the wire.
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

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        Err(ModelListingUnsupportedError.into())
    }

    /// Fetch the list of available models with pricing data for this
    /// model_provider. Default delegates to `list_models` and returns no
    /// pricing. Concrete providers that receive pricing from their `/models`
    /// endpoint override this to return enriched data.
    async fn list_models_with_pricing(&self) -> anyhow::Result<Vec<ModelInfo>> {
        Ok(self
            .list_models()
            .await?
            .into_iter()
            .map(|id| ModelInfo {
                id,
                pricing: None,
                context_window: None,
            })
            .collect())
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
                        "ModelProvider returned non-prompt-guided tools payload ({payload:?}) while supports_native_tools() is false"
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
            let response = ChatResponse {
                output_blocks: Vec::new(),
                text: Some(text),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            };
            return (!response.is_semantically_empty_terminal())
                .then_some(response)
                .ok_or_else(|| anyhow::Error::new(SemanticEmptyTerminalCompletion));
        }

        let text = self
            .chat_with_history(request.messages, model, temperature)
            .await?;
        let response = ChatResponse {
            output_blocks: Vec::new(),
            text: Some(text),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        };
        (!response.is_semantically_empty_terminal())
            .then_some(response)
            .ok_or_else(|| anyhow::Error::new(SemanticEmptyTerminalCompletion))
    }

    /// Whether model_provider supports native tool calls over API.
    fn supports_native_tools(&self) -> bool {
        self.capabilities().native_tool_calling
    }

    /// Whether model_provider supports multimodal vision input.
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
        let response = ChatResponse {
            output_blocks: Vec::new(),
            text: Some(text),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        };
        (!response.is_semantically_empty_terminal())
            .then_some(response)
            .ok_or_else(|| anyhow::Error::new(SemanticEmptyTerminalCompletion))
    }

    /// Whether model_provider supports streaming responses.
    fn supports_streaming(&self) -> bool {
        false
    }

    /// Whether model_provider can emit structured tool-call stream events.
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

/// Blanket implementation: `Arc<T>` delegates all `ModelProvider` methods to `T`.
/// This eliminates the need for manual `impl ModelProvider for Arc<MyModelProvider>`
/// boilerplate in test and production code.
#[async_trait]
impl<T: ModelProvider + ?Sized> ModelProvider for Arc<T> {
    fn supports_single_tool_rounds(&self, model: &str) -> bool {
        self.as_ref().supports_single_tool_rounds(model)
    }

    async fn chat_with_tool_round_policy(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
        policy: ToolRoundPolicy,
    ) -> anyhow::Result<ChatResponse> {
        self.as_ref()
            .chat_with_tool_round_policy(request, model, temperature, policy)
            .await
    }

    fn stream_chat_with_tool_round_policy(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
        policy: ToolRoundPolicy,
    ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
        self.as_ref().stream_chat_with_tool_round_policy(
            request,
            model,
            temperature,
            options,
            policy,
        )
    }

    fn dispatch_reference(&self) -> &str {
        self.as_ref().dispatch_reference()
    }

    fn is_dispatch_composite(&self) -> bool {
        self.as_ref().is_dispatch_composite()
    }

    fn has_stable_request_identity(&self, model: &str) -> bool {
        self.as_ref().has_stable_request_identity(model)
    }

    fn supports_exact_request_replay(&self, request: ChatRequest<'_>, model: &str) -> bool {
        self.as_ref().supports_exact_request_replay(request, model)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.as_ref().capabilities()
    }

    fn capabilities_for_model(&self, model: &str) -> ProviderCapabilities {
        self.as_ref().capabilities_for_model(model)
    }

    fn vision_limited_by(&self, model: &str) -> Option<String> {
        self.as_ref().vision_limited_by(model)
    }

    fn has_mixed_native_tool_support_for_model(&self, model: &str) -> bool {
        self.as_ref().has_mixed_native_tool_support_for_model(model)
    }

    fn default_max_tokens(&self) -> u32 {
        self.as_ref().default_max_tokens()
    }

    fn default_temperature(&self) -> f64 {
        self.as_ref().default_temperature()
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

#[cfg(test)]
mod capability_tests {
    use super::ModelProvider;
    use crate::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
    use async_trait::async_trait;

    struct NativeAccessorOnlyProvider;

    impl Attributable for NativeAccessorOnlyProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }

        fn alias(&self) -> &str {
            "native_accessor_only"
        }
    }

    #[async_trait]
    impl ModelProvider for NativeAccessorOnlyProvider {
        fn supports_native_tools(&self) -> bool {
            true
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }
    }

    #[tokio::test]
    async fn default_model_listing_returns_typed_unsupported_error() {
        let error = NativeAccessorOnlyProvider
            .list_models()
            .await
            .expect_err("default model listing must be unsupported");
        assert!(
            error
                .downcast_ref::<super::ModelListingUnsupportedError>()
                .is_some(),
            "default listing error must preserve the typed unsupported marker: {error}"
        );
    }

    #[test]
    fn model_capabilities_preserve_native_accessor_overrides() {
        let provider = NativeAccessorOnlyProvider;

        assert!(
            !provider.capabilities().native_tool_calling,
            "the fixture must exercise the legacy accessor-only override"
        );
        assert!(
            provider
                .capabilities_for_model("requested-model")
                .native_tool_calling,
            "model-aware capability lookup must preserve legacy supports_native_tools overrides"
        );
    }
}

#[cfg(test)]
mod turn_order_tests {
    use super::{
        ChatMessage, ChatResponse, ProviderMediaSource, ProviderOutputBlock, ToolCall,
        ToolResultMessage,
    };

    #[test]
    fn structured_output_obeys_terminal_content_semantics() {
        let mut response = ChatResponse {
            text: None,
            output_blocks: Vec::new(),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        };
        for text in [
            " \n\t",
            "<think>private reasoning</think> ",
            "<think>unfinished",
        ] {
            response.output_blocks = vec![ProviderOutputBlock::Text { text: text.into() }];
            assert!(response.is_semantically_empty_terminal(), "{text}");
        }
        response.output_blocks = vec![ProviderOutputBlock::Text {
            text: "answer".into(),
        }];
        assert!(!response.is_semantically_empty_terminal());
        response.output_blocks = vec![ProviderOutputBlock::Media {
            media_type: "image".into(),
            format: Some("image/png".into()),
            source: ProviderMediaSource::Bytes {
                bytes: vec![1, 2, 3],
            },
        }];
        assert!(!response.is_semantically_empty_terminal());
    }

    #[test]
    fn semantic_empty_terminal_ignores_reasoning_content() {
        let response = ChatResponse {
            output_blocks: Vec::new(),
            text: Some("  \n".to_string()),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: Some("internal reasoning".to_string()),
        };

        assert!(response.is_semantically_empty_terminal());
    }

    #[test]
    fn semantic_empty_terminal_uses_display_text_after_think_tag_stripping() {
        let response = ChatResponse {
            output_blocks: Vec::new(),
            text: Some("<think>internal reasoning</think>".to_string()),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        };

        assert!(response.is_semantically_empty_terminal());
    }

    #[test]
    fn semantic_empty_terminal_keeps_tool_only_response_valid() {
        let response = ChatResponse {
            output_blocks: Vec::new(),
            text: None,
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                arguments: "{}".to_string(),
                extra_content: None,
            }],
            usage: None,
            reasoning_content: None,
        };

        assert!(!response.is_semantically_empty_terminal());
    }

    #[test]
    fn text_response_is_not_semantically_empty() {
        let response = ChatResponse {
            output_blocks: Vec::new(),
            text: Some("done".to_string()),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        };

        assert!(!response.is_semantically_empty_terminal());
    }

    #[test]
    fn tool_only_response_is_not_semantically_empty() {
        let response = ChatResponse {
            output_blocks: Vec::new(),
            text: None,
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                arguments: "{}".to_string(),
                extra_content: None,
            }],
            usage: None,
            reasoning_content: None,
        };

        assert!(!response.is_semantically_empty_terminal());
    }

    #[test]
    fn drops_leading_assistant_tool_call_before_first_user() {
        let mut msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::assistant("[tool_call] fire"),
            ChatMessage::tool_result(ToolResultMessage {
                tool_call_id: "orphan".into(),
                tool_name: "shell".into(),
                success: Some(true),
                content: "result".into(),
                attachments: Vec::new(),
            }),
            ChatMessage::user("actual user"),
        ];
        ChatMessage::sanitize_leading_turn_order(&mut msgs);
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs[1].role, "user");
        assert_eq!(msgs[1].content, "actual user");
    }

    #[test]
    fn drops_leading_orphan_tool_turn() {
        let mut msgs = vec![
            ChatMessage::tool_result(ToolResultMessage {
                tool_call_id: "orphan".into(),
                tool_name: "shell".into(),
                success: Some(true),
                content: "orphan result".into(),
                attachments: Vec::new(),
            }),
            ChatMessage::user("hi"),
        ];
        ChatMessage::sanitize_leading_turn_order(&mut msgs);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
    }

    #[test]
    fn preserves_already_valid_history() {
        let mut msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("q"),
            ChatMessage::assistant("[tool_call] x"),
            ChatMessage::tool_result(ToolResultMessage {
                tool_call_id: "call".into(),
                tool_name: "shell".into(),
                success: Some(true),
                content: "r".into(),
                attachments: Vec::new(),
            }),
            ChatMessage::assistant("done"),
        ];
        let before = msgs.clone();
        ChatMessage::sanitize_leading_turn_order(&mut msgs);
        assert_eq!(msgs.len(), before.len());
        assert_eq!(msgs[1].role, "user");
    }

    #[test]
    fn no_user_turn_drops_all_non_system() {
        let mut msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::assistant("[tool_call] x"),
            ChatMessage::tool_result(ToolResultMessage {
                tool_call_id: "call".into(),
                tool_name: "shell".into(),
                success: Some(true),
                content: "r".into(),
                attachments: Vec::new(),
            }),
        ];
        ChatMessage::sanitize_leading_turn_order(&mut msgs);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "system");
    }

    #[test]
    fn empty_history_is_noop() {
        let mut msgs: Vec<ChatMessage> = vec![];
        ChatMessage::sanitize_leading_turn_order(&mut msgs);
        assert!(msgs.is_empty());
    }
}

#[cfg(test)]
mod projected_entry_count_tests {
    use super::projected_entry_count;
    use super::{ChatMessage, ConversationMessage, ToolCall, ToolResultMessage};

    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: "{}".into(),
            extra_content: None,
        }
    }

    fn result(id: &str) -> ToolResultMessage {
        ToolResultMessage {
            success: None,
            tool_call_id: id.into(),
            content: "out".into(),
            tool_name: "shell".into(),
            attachments: Vec::new(),
        }
    }

    fn batch(text: Option<&str>, calls: Vec<ToolCall>) -> ConversationMessage {
        ConversationMessage::AssistantToolCalls {
            text: text.map(str::to_string),
            tool_calls: calls,
            reasoning_content: None,
        }
    }

    #[test]
    fn counts_chat_and_call_batches_by_their_parts() {
        let count = projected_entry_count(&[
            ConversationMessage::Chat(ChatMessage::user("hi")),
            batch(Some("working"), vec![call("a", "shell"), call("b", "read")]),
            batch(Some(""), vec![call("c", "read")]),
            batch(None, vec![]),
        ]);
        // 1 chat + (text + 2 calls) + (empty text, 1 call) + (nothing).
        assert_eq!(count, 5);
    }

    #[test]
    fn results_fold_into_open_calls_and_count_orphans() {
        let count = projected_entry_count(&[
            batch(None, vec![call("dup", "shell"), call("dup", "shell")]),
            ConversationMessage::ToolResults(vec![
                result("dup"),
                result("dup"),
                result("dup"), // one more result than calls
                result("never-issued"),
            ]),
        ]);
        // 2 calls, second batch's extra results are orphans.
        assert_eq!(count, 4);
    }
}

#[cfg(test)]
mod thinking_display_tests {
    use super::{NativeThinkingParams, ThinkingDisplay};

    #[test]
    fn as_str_maps_updates_variant() {
        assert_eq!(ThinkingDisplay::Updates.as_str(), "updates");
    }

    #[test]
    fn serialization_includes_display_when_present() {
        let params = NativeThinkingParams {
            budget_tokens: 1_024,
            display: Some(ThinkingDisplay::Updates),
        };
        let json = serde_json::to_string(&params).expect("serialization should succeed");
        assert!(
            json.contains("\"display\":\"updates\""),
            "expected display field in serialized params, got: {json}"
        );
    }

    #[test]
    fn serialization_omits_display_when_absent() {
        let params = NativeThinkingParams {
            budget_tokens: 1_024,
            display: None,
        };
        let json = serde_json::to_string(&params).expect("serialization should succeed");
        assert!(
            !json.contains("display"),
            "expected display field to be omitted, got: {json}"
        );
    }
}
