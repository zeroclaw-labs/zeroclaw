use crate::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    ModelProvider, ProviderCapabilities, StreamChunk, StreamError, StreamEvent, StreamOptions,
    StreamResult, TokenUsage, ToolCall as ProviderToolCall,
};
use anyhow::Context;
use async_trait::async_trait;
use base64::Engine as _;
use futures_util::stream::{self, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use zeroclaw_api::tool::ToolSpec;

/// Anthropic's API documentation lists 1.0 as the default sampling temperature.
const TEMPERATURE_DEFAULT: f64 = 1.0;
/// Anthropic's public API endpoint. Overrideable via `model_providers.<name>.base_url`.
pub(crate) const BASE_URL: &str = "https://api.anthropic.com";
/// Max wait for the next SSE line before the stream is treated as stalled.
/// reqwest's overall `.timeout()` does not reliably fire once a streaming body
/// is being drained chunk-by-chunk, so a connection that goes silent after
/// `message_start` (proxy/load-balancer hiccup) parks `next_line().await`
/// forever — the detached parser task leaks and the turn hangs on "working".
/// A per-line idle bound converts that into a retryable `StreamError`.
const SSE_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

use crate::stream_guard::AbortOnDrop;

pub struct AnthropicModelProvider {
    /// `[providers.models.anthropic.<alias>]` config-key alias.
    alias: String,
    credential: Option<String>,
    base_url: String,
    max_tokens: u32,
}

#[cfg(test)]
#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
}

#[cfg(test)]
#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
}

#[cfg(test)]
#[derive(Debug, Deserialize)]
struct ChatResponse {
    content: Vec<ContentBlock>,
}

#[cfg(test)]
#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Serialize)]
struct NativeChatRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<SystemPrompt>,
    messages: Vec<NativeMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<NativeToolSpec>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<NativeThinkingConfig>,
}

#[derive(Debug, Serialize)]
struct NativeThinkingConfig {
    #[serde(rename = "type")]
    kind: &'static str,
    budget_tokens: u32,
}

/// Whether a model accepts the fixed-budget native-thinking request shape
/// (`{"thinking": {"type": "enabled", "budget_tokens": N}}`). Opus 4.7 supports
/// only adaptive thinking and rejects fixed budgets with a 400; until adaptive
/// thinking is implemented, those models stay on prompt-based reasoning.
/// Anthropic's extended-thinking docs:
/// <https://platform.claude.com/docs/en/build-with-claude/extended-thinking>
fn anthropic_model_supports_native_thinking(model: &str) -> bool {
    !model.contains("claude-opus-4-7")
}

#[derive(Debug, Serialize)]
struct NativeMessage {
    role: String,
    content: Vec<NativeContentOut>,
}

#[derive(Debug, Serialize)]
struct ImageSource {
    #[serde(rename = "type")]
    source_type: String,
    media_type: String,
    data: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum NativeContentOut {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "image")]
    Image { source: ImageSource },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    /// Thinking block for round-tripping extended thinking in conversation
    /// history. Required when thinking is enabled and assistant messages
    /// contain tool_use blocks.
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
}

#[derive(Debug, Serialize)]
struct NativeToolSpec {
    name: String,
    description: String,
    /// `Arc`-shared with the tool registry's stored schema when no cleaning
    /// is required — serialized transparently, deep-cloned only for schemas
    /// the Anthropic cleaner actually rewrites (#8642).
    input_schema: std::sync::Arc<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Serialize)]
struct CacheControl {
    #[serde(rename = "type")]
    cache_type: String,
}

impl CacheControl {
    fn ephemeral() -> Self {
        Self {
            cache_type: "ephemeral".to_string(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum SystemPrompt {
    String(String),
    Blocks(Vec<SystemBlock>),
}

#[derive(Debug, Serialize)]
struct SystemBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

#[derive(Debug, Deserialize)]
struct NativeChatResponse {
    #[serde(default)]
    content: Vec<NativeContentIn>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    /// Tokens *after* the last cache breakpoint — NOT the total prompt.
    /// Per Anthropic prompt-caching docs:
    /// total_input = cache_read + cache_creation + input_tokens.
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    /// Tokens served from the prompt cache this request.
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    /// Tokens written to the prompt cache this request (cache miss path).
    /// Disjoint from `cache_read_input_tokens` and `input_tokens`.
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct NativeContentIn {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    /// Signature for integrity verification of thinking blocks.
    #[serde(default)]
    signature: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<serde_json::Value>,
}

impl AnthropicModelProvider {
    pub fn new(alias: &str, credential: Option<&str>) -> Self {
        Self::with_base_url(alias, credential, None)
    }

    pub fn with_base_url(alias: &str, credential: Option<&str>, base_url: Option<&str>) -> Self {
        let base_url = base_url
            .map(|u| u.trim_end_matches('/'))
            .unwrap_or(BASE_URL)
            .to_string();
        Self {
            alias: alias.to_string(),
            credential: credential
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .map(ToString::to_string),
            base_url,
            max_tokens: zeroclaw_api::model_provider::BASELINE_MAX_TOKENS,
        }
    }

    /// Override the maximum output tokens for API requests.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    fn is_setup_token(token: &str) -> bool {
        token.starts_with("sk-ant-oat01-")
    }

    fn apply_auth(
        &self,
        request: reqwest::RequestBuilder,
        credential: &str,
    ) -> reqwest::RequestBuilder {
        let is_setup = Self::is_setup_token(credential);
        // Diagnostic for "401 invalid x-api-key" mysteries: when a provider
        // is sending a credential the upstream rejects, this is the only
        // line that nails what bytes actually went out. Logs header kind,
        // length, first 8 chars (enough to identify api03 vs oat01 vs an
        // accidental enc2: blob) and last 4 (smudge for tail integrity).
        // No full credential — that stays out of logs.
        let len = credential.len();
        let head: String = credential.chars().take(8).collect();
        let tail: String = credential
            .chars()
            .rev()
            .take(4)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"header": if is_setup { "Authorization" } else { "x-api-key" }, "credential_len": len, "credential_head": head, "credential_tail": tail})), "Anthropic auth header applied");
        if is_setup {
            request
                .header("Authorization", format!("Bearer {credential}"))
                .header(
                    "anthropic-beta",
                    "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14",
                )
                .header("anthropic-dangerous-direct-browser-access", "true")
        } else {
            request.header("x-api-key", credential)
        }
    }

    /// For OAuth tokens, Anthropic requires the system prompt to start with the
    /// Claude Code identity prefix. This prepends it to any existing system prompt.
    fn apply_oauth_system_prompt(system: Option<SystemPrompt>) -> Option<SystemPrompt> {
        let prefix = SystemBlock {
            block_type: "text".to_string(),
            text: "You are Claude Code, Anthropic's official CLI for Claude.".to_string(),
            cache_control: Some(CacheControl::ephemeral()),
        };
        match system {
            Some(SystemPrompt::Blocks(mut blocks)) => {
                blocks.insert(0, prefix);
                Some(SystemPrompt::Blocks(blocks))
            }
            Some(SystemPrompt::String(s)) => Some(SystemPrompt::Blocks(vec![
                prefix,
                SystemBlock {
                    block_type: "text".to_string(),
                    text: s,
                    cache_control: Some(CacheControl::ephemeral()),
                },
            ])),
            None => Some(SystemPrompt::Blocks(vec![prefix])),
        }
    }

    /// Cache conversations with more than 1 non-system message (i.e. after first exchange)
    fn should_cache_conversation(messages: &[ChatMessage]) -> bool {
        messages.iter().filter(|m| m.role != "system").count() > 1
    }

    /// Apply cache control to the last message content block
    fn apply_cache_to_last_message(messages: &mut [NativeMessage]) {
        if let Some(last_msg) = messages.last_mut()
            && let Some(last_content) = last_msg.content.last_mut()
        {
            match last_content {
                NativeContentOut::Text { cache_control, .. }
                | NativeContentOut::ToolResult { cache_control, .. } => {
                    *cache_control = Some(CacheControl::ephemeral());
                }
                NativeContentOut::ToolUse { .. }
                | NativeContentOut::Image { .. }
                | NativeContentOut::Thinking { .. } => {}
            }
        }
    }

    fn convert_tools(tools: Option<&[ToolSpec]>) -> Option<Vec<NativeToolSpec>> {
        let items = tools?;
        if items.is_empty() {
            return None;
        }
        let mut native_tools: Vec<NativeToolSpec> = items
            .iter()
            .map(|tool| NativeToolSpec {
                name: tool.name.clone(),
                description: tool.description.clone(),
                input_schema: zeroclaw_api::schema::SchemaCleanr::clean_shared(
                    &tool.parameters,
                    zeroclaw_api::schema::CleaningStrategy::Anthropic,
                ),
                cache_control: None,
            })
            .collect();

        // Cache the last tool definition (caches all tools)
        if let Some(last_tool) = native_tools.last_mut() {
            last_tool.cache_control = Some(CacheControl::ephemeral());
        }

        Some(native_tools)
    }

    fn parse_assistant_tool_call_message(content: &str) -> Option<Vec<NativeContentOut>> {
        let value = serde_json::from_str::<serde_json::Value>(content).ok()?;
        let tool_calls = value
            .get("tool_calls")
            .and_then(|v| serde_json::from_value::<Vec<ProviderToolCall>>(v.clone()).ok())?;

        let mut blocks = Vec::new();

        // When extended thinking is enabled, assistant messages must start
        // with thinking blocks (including signatures) before any tool_use
        // blocks. The reasoning_content field stores JSON-encoded thinking
        // blocks from the original response.
        if let Some(reasoning) = value
            .get("reasoning_content")
            .and_then(serde_json::Value::as_str)
            .filter(|r| !r.is_empty())
        {
            for part in reasoning.split('\n') {
                if let Ok(block) = serde_json::from_str::<serde_json::Value>(part) {
                    let thinking = block
                        .get("thinking")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string();
                    let signature = block
                        .get("signature")
                        .and_then(|s| s.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string());
                    blocks.push(NativeContentOut::Thinking {
                        thinking,
                        signature,
                    });
                }
            }
        }

        if let Some(text) = value
            .get("content")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            blocks.push(NativeContentOut::Text {
                text: text.to_string(),
                cache_control: None,
            });
        }
        for call in tool_calls {
            let input = serde_json::from_str::<serde_json::Value>(&call.arguments)
                .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
            blocks.push(NativeContentOut::ToolUse {
                id: call.id,
                name: call.name,
                input,
                cache_control: None,
            });
        }
        Some(blocks)
    }

    fn parse_tool_result_message(content: &str) -> Option<NativeMessage> {
        let value = serde_json::from_str::<serde_json::Value>(content).ok()?;
        let tool_use_id = value
            .get("tool_call_id")
            .and_then(serde_json::Value::as_str)?
            .to_string();
        let result = value
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        Some(NativeMessage {
            role: "user".to_string(),
            content: vec![NativeContentOut::ToolResult {
                tool_use_id,
                content: result,
                cache_control: None,
            }],
        })
    }

    fn convert_messages(messages: &[ChatMessage]) -> (Option<SystemPrompt>, Vec<NativeMessage>) {
        let mut system_text = None;
        let mut native_messages = Vec::new();

        for (index, msg) in messages.iter().enumerate() {
            if ChatMessage::should_skip_internal_pruning_marker(messages, index) {
                continue;
            }
            match msg.role.as_str() {
                "system" => {
                    if system_text.is_none() {
                        system_text = Some(msg.content.clone());
                    }
                }
                "assistant" => {
                    if let Some(blocks) = Self::parse_assistant_tool_call_message(&msg.content) {
                        native_messages.push(NativeMessage {
                            role: "assistant".to_string(),
                            content: blocks,
                        });
                    } else if !msg.content.trim().is_empty() {
                        native_messages.push(NativeMessage {
                            role: "assistant".to_string(),
                            content: vec![NativeContentOut::Text {
                                text: msg.content.clone(),
                                cache_control: None,
                            }],
                        });
                    }
                }
                "tool" => {
                    let tool_msg = if let Some(tr) = Self::parse_tool_result_message(&msg.content) {
                        tr
                    } else if !msg.content.trim().is_empty() {
                        NativeMessage {
                            role: "user".to_string(),
                            content: vec![NativeContentOut::Text {
                                text: msg.content.clone(),
                                cache_control: None,
                            }],
                        }
                    } else {
                        continue;
                    };
                    // Tool results map to role "user"; merge consecutive ones
                    // into a single message so Anthropic doesn't reject the
                    // request for having adjacent same-role messages.
                    if native_messages
                        .last()
                        .is_some_and(|m| m.role == tool_msg.role)
                    {
                        native_messages
                            .last_mut()
                            .unwrap()
                            .content
                            .extend(tool_msg.content);
                    } else {
                        native_messages.push(tool_msg);
                    }
                }
                _ => {
                    // Parse image markers from user message content
                    let (text, image_refs) = crate::multimodal::parse_image_markers(&msg.content);
                    let mut content_blocks: Vec<NativeContentOut> = Vec::new();

                    // Add image content blocks for each image reference
                    for img_ref in &image_refs {
                        let (media_type, data) = if img_ref.starts_with("data:") {
                            // Data URI format: data:image/jpeg;base64,/9j/4AAQ...
                            if let Some(comma) = img_ref.find(',') {
                                let header = &img_ref[5..comma];
                                let mime =
                                    header.split(';').next().unwrap_or("image/jpeg").to_string();
                                let b64 = img_ref[comma + 1..].trim().to_string();
                                (mime, b64)
                            } else {
                                continue;
                            }
                        } else if std::path::Path::new(img_ref.trim()).exists() {
                            // Local file path
                            match std::fs::read(img_ref.trim()) {
                                Ok(bytes) => {
                                    let b64 =
                                        base64::engine::general_purpose::STANDARD.encode(&bytes);
                                    let ext = std::path::Path::new(img_ref.trim())
                                        .extension()
                                        .and_then(|e| e.to_str())
                                        .unwrap_or("jpg");
                                    let mime = match ext {
                                        "png" => "image/png",
                                        "gif" => "image/gif",
                                        "webp" => "image/webp",
                                        _ => "image/jpeg",
                                    }
                                    .to_string();
                                    (mime, b64)
                                }
                                Err(_) => continue,
                            }
                        } else {
                            continue;
                        };

                        content_blocks.push(NativeContentOut::Image {
                            source: ImageSource {
                                source_type: "base64".to_string(),
                                media_type,
                                data,
                            },
                        });
                    }

                    // Add text content block (skip empty text when images are present)
                    if text.is_empty() && !image_refs.is_empty() {
                        content_blocks.push(NativeContentOut::Text {
                            text: "[image]".to_string(),
                            cache_control: None,
                        });
                    } else if !text.trim().is_empty() {
                        content_blocks.push(NativeContentOut::Text {
                            text,
                            cache_control: None,
                        });
                    }

                    // Merge into previous user message if present (e.g.
                    // when a user message immediately follows tool results
                    // which are also role "user" in Anthropic's format).
                    if native_messages.last().is_some_and(|m| m.role == "user") {
                        native_messages
                            .last_mut()
                            .unwrap()
                            .content
                            .extend(content_blocks);
                    } else {
                        native_messages.push(NativeMessage {
                            role: "user".to_string(),
                            content: content_blocks,
                        });
                    }
                }
            }
        }

        Self::backfill_orphaned_tool_uses(&mut native_messages);

        // Always use Blocks format with cache_control for system prompts
        let system_prompt = system_text.map(|text| {
            SystemPrompt::Blocks(vec![SystemBlock {
                block_type: "text".to_string(),
                text,
                cache_control: Some(CacheControl::ephemeral()),
            }])
        });

        (system_prompt, native_messages)
    }

    /// Pair any orphaned `tool_use` with a stub `tool_result` so interrupted
    /// turns can't wedge the session with a hard 400 on replay. Defensive
    /// backstop for the canonical-history guard in the runtime.
    fn backfill_orphaned_tool_uses(messages: &mut Vec<NativeMessage>) {
        let mut idx = 0;
        while idx < messages.len() {
            let pending: Vec<String> = messages[idx]
                .content
                .iter()
                .filter_map(|block| match block {
                    NativeContentOut::ToolUse { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .collect();

            if pending.is_empty() {
                idx += 1;
                continue;
            }

            let answered: std::collections::HashSet<String> = messages
                .get(idx + 1)
                .map(|next| {
                    next.content
                        .iter()
                        .filter_map(|block| match block {
                            NativeContentOut::ToolResult { tool_use_id, .. } => {
                                Some(tool_use_id.clone())
                            }
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();

            let stubs: Vec<NativeContentOut> = pending
                .into_iter()
                .filter(|id| !answered.contains(id))
                .map(|tool_use_id| NativeContentOut::ToolResult {
                    tool_use_id,
                    content: "[tool result missing from history — the turn was \
                              interrupted before this tool finished]"
                        .to_string(),
                    cache_control: None,
                })
                .collect();

            if !stubs.is_empty() {
                if messages
                    .get(idx + 1)
                    .is_some_and(|next| next.role == "user")
                {
                    let next = &mut messages[idx + 1];
                    let mut merged = stubs;
                    merged.append(&mut next.content);
                    next.content = merged;
                } else {
                    messages.insert(
                        idx + 1,
                        NativeMessage {
                            role: "user".to_string(),
                            content: stubs,
                        },
                    );
                }
            }

            idx += 1;
        }
    }

    fn parse_native_response(response: NativeChatResponse) -> ProviderChatResponse {
        let mut text_parts = Vec::new();
        let mut thinking_parts = Vec::new();
        let mut tool_calls = Vec::new();

        let usage = response.usage.map(|u| {
            // Anthropic's three input buckets are DISJOINT per
            // https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching
            //
            //   total_input_tokens = cache_read_input_tokens
            //                      + cache_creation_input_tokens
            //                      + input_tokens
            //
            // Anthropic's `input_tokens` is the tokens AFTER the last cache
            // breakpoint, not the total prompt. The other two are tokens
            // before the breakpoint (read from cache vs. being written now).
            //
            // Our internal TokenUsage contract is that `input_tokens` is the
            // *total* prompt sent to the model. Sum all three to normalize.
            // `cached_input_tokens` is reported as the cache-read portion
            // (the discounted-billing subset of the total) — this matches
            // what billable_uncached_input = input - cached expects.
            let uncached = u.input_tokens.unwrap_or(0);
            let cache_read = u.cache_read_input_tokens.unwrap_or(0);
            let cache_create = u.cache_creation_input_tokens.unwrap_or(0);
            let total = uncached
                .saturating_add(cache_read)
                .saturating_add(cache_create);
            let any_reported = u.input_tokens.is_some()
                || u.cache_read_input_tokens.is_some()
                || u.cache_creation_input_tokens.is_some();
            TokenUsage {
                input_tokens: if any_reported { Some(total) } else { None },
                output_tokens: u.output_tokens,
                cached_input_tokens: u.cache_read_input_tokens,
            }
        });

        for block in response.content {
            match block.kind.as_str() {
                "text" => {
                    if let Some(text) = block.text.map(|t| t.trim().to_string())
                        && !text.is_empty()
                    {
                        text_parts.push(text);
                    }
                }
                "thinking" => {
                    // Store thinking text byte-for-byte: the signature is
                    // computed over the exact bytes the model returned, so
                    // any mutation (including trim()) invalidates it on
                    // replay. Only skip when the provider returns genuinely
                    // empty content.
                    if let Some(thinking) = block.thinking.as_deref().or(block.text.as_deref())
                        && !thinking.is_empty()
                    {
                        let json_block = serde_json::json!({
                            "thinking": thinking,
                            "signature": block.signature.as_deref().unwrap_or(""),
                        });
                        thinking_parts.push(json_block.to_string());
                    }
                }
                "tool_use" => {
                    let name = block.name.unwrap_or_default();
                    if name.is_empty() {
                        continue;
                    }
                    let arguments = block
                        .input
                        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
                    tool_calls.push(ProviderToolCall {
                        id: block.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                        name,
                        arguments: arguments.to_string(),
                        extra_content: None,
                    });
                }
                _ => {}
            }
        }

        let reasoning_content = if thinking_parts.is_empty() {
            None
        } else {
            Some(thinking_parts.join("\n"))
        };

        ProviderChatResponse {
            text: if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.join("\n"))
            },
            tool_calls,
            usage,
            reasoning_content,
        }
    }

    /// Resolve thinking parameters for an API request. Returns the effective
    /// temperature (forced to 1.0 when thinking is active), the thinking
    /// config for the request body, and the effective max_tokens (raised to
    /// meet budget_tokens minimum when needed).
    fn resolve_thinking(
        &self,
        thinking: Option<zeroclaw_api::model_provider::NativeThinkingParams>,
        temperature: Option<f64>,
        model: &str,
    ) -> (Option<f64>, Option<NativeThinkingConfig>, u32) {
        match thinking {
            Some(params) if anthropic_model_supports_native_thinking(model) => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"budget_tokens": params.budget_tokens})),
                    "Native extended thinking enabled; forcing temperature=1.0"
                );
                // API requires max_tokens > budget_tokens (strictly greater).
                let min_required = params.budget_tokens + 1;
                let max_tokens = self.max_tokens.max(min_required);
                (
                    Some(1.0),
                    Some(NativeThinkingConfig {
                        kind: "enabled",
                        budget_tokens: params.budget_tokens,
                    }),
                    max_tokens,
                )
            }
            Some(_) => {
                // Caller asked for native thinking but the model rejects the
                // fixed-budget request shape. Drop to prompt-based reasoning
                // (the agent loop's prefix already injected) and keep the
                // caller-supplied temperature so per-model guards still apply.
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"model": model})),
                    "Native extended thinking requested but model only supports adaptive thinking; falling back to prompt-based reasoning"
                );
                (temperature, None, self.max_tokens)
            }
            None => (temperature, None, self.max_tokens),
        }
    }

    fn http_client(&self) -> Client {
        zeroclaw_config::schema::build_runtime_proxy_client_with_timeouts(
            "model_provider.anthropic",
            120,
            10,
        )
    }

    /// Build a streaming request body from a `NativeChatRequest`.
    fn build_streaming_request(request: &NativeChatRequest) -> anyhow::Result<serde_json::Value> {
        let mut body = serde_json::to_value(request)
            .context("Failed to serialize NativeChatRequest to JSON")?;
        body["stream"] = serde_json::Value::Bool(true);
        Ok(body)
    }

    /// Parse Anthropic SSE lines from `response` and send `StreamEvent`s to `tx`.
    async fn parse_anthropic_sse(
        response: reqwest::Response,
        tx: &tokio::sync::mpsc::Sender<StreamResult<StreamEvent>>,
    ) {
        use tokio_util::io::StreamReader;

        let byte_stream = response
            .bytes_stream()
            .map(|result| result.map_err(std::io::Error::other));
        let reader = StreamReader::new(byte_stream);
        Self::parse_anthropic_sse_from_reader(reader, tx).await;
    }

    /// Inner loop split out of `parse_anthropic_sse` so unit tests can feed a
    /// `Cursor<&[u8]>` directly without spinning up a mock HTTP server.
    async fn parse_anthropic_sse_from_reader<R>(
        reader: R,
        tx: &tokio::sync::mpsc::Sender<StreamResult<StreamEvent>>,
    ) where
        R: tokio::io::AsyncBufRead + Unpin,
    {
        use tokio::io::AsyncBufReadExt;

        let mut lines = reader.lines();

        let mut tool_id: Option<String> = None;
        let mut tool_name: Option<String> = None;
        let mut tool_input_json = String::new();

        // Anthropic emits usage in two places: `message_start` carries the
        // input-token count + prompt-cache reads; `message_delta` carries
        // running output-token totals (each delta supersedes the prior). We
        // capture both, then emit one `StreamEvent::Usage` at `message_stop`
        // so the gateway accumulator and `record_turn_cost()` see the same
        // signal Anthropic sends — closes the original #6001 live repro,
        // which was Anthropic-shaped streaming.
        let mut input_tokens: Option<u64> = None;
        let mut output_tokens: Option<u64> = None;
        let mut cached_input_tokens: Option<u64> = None;
        let mut cache_creation_input_tokens: Option<u64> = None;

        let mut saw_stop_reason = false;

        loop {
            let line = match tokio::time::timeout(SSE_IDLE_TIMEOUT, lines.next_line()).await {
                Ok(Ok(Some(line))) => line,
                Ok(Ok(None)) => break,
                Ok(Err(err)) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_category(::zeroclaw_log::EventCategory::Provider)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "error": format!("{err}"),
                            })),
                        "stream: SSE read error — aborting stream"
                    );
                    let _ = tx
                        .send(Err(StreamError::Http(format!("SSE read error: {err}"))))
                        .await;
                    return;
                }
                Err(_) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "idle_secs": SSE_IDLE_TIMEOUT.as_secs(),
                            })),
                        "stream: SSE idle timeout — connection stalled, aborting stream"
                    );
                    let _ = tx
                        .send(Err(StreamError::Http(format!(
                            "SSE stream stalled: no data for {}s",
                            SSE_IDLE_TIMEOUT.as_secs()
                        ))))
                        .await;
                    return;
                }
            };
            let line = line.trim().to_string();
            if !line.starts_with("data: ") {
                continue;
            }
            let json_str = &line["data: ".len()..];

            let event: serde_json::Value = match serde_json::from_str(json_str) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let event_type = event
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or_default();

            match event_type {
                "message_start" => {
                    let model = event
                        .get("message")
                        .and_then(|m| m.get("model"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("unknown");
                    let usage = event.get("message").and_then(|m| m.get("usage"));
                    let observed_input = usage
                        .and_then(|u| u.get("input_tokens"))
                        .and_then(|t| t.as_u64());
                    let observed_cached = usage
                        .and_then(|u| u.get("cache_read_input_tokens"))
                        .and_then(|t| t.as_u64());
                    let observed_cache_create = usage
                        .and_then(|u| u.get("cache_creation_input_tokens"))
                        .and_then(|t| t.as_u64());
                    if let Some(v) = observed_input {
                        input_tokens = Some(v);
                    }
                    if let Some(v) = observed_cached {
                        cached_input_tokens = Some(v);
                    }
                    if let Some(v) = observed_cache_create {
                        cache_creation_input_tokens = Some(v);
                    }
                    ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"model": model, "input_tokens": observed_input, "cached_input_tokens": observed_cached, "cache_creation_input_tokens": observed_cache_create})), "stream: message_start");
                }
                "content_block_start" => {
                    if let Some(block) = event.get("content_block") {
                        let block_type = block
                            .get("type")
                            .and_then(|t| t.as_str())
                            .unwrap_or_default();
                        if block_type == "tool_use" {
                            if let Some(id) = tool_id.take() {
                                let name = tool_name.take().unwrap_or_default();
                                let input = std::mem::take(&mut tool_input_json);
                                let _ = tx
                                    .send(Ok(StreamEvent::ToolCall(ProviderToolCall {
                                        id,
                                        name,
                                        arguments: input,
                                        extra_content: None,
                                    })))
                                    .await;
                            }
                            tool_id = block
                                .get("id")
                                .and_then(|v| v.as_str())
                                .map(ToString::to_string);
                            tool_name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .map(ToString::to_string);
                            tool_input_json.clear();
                        }
                    }
                }
                "content_block_delta" => {
                    if let Some(delta) = event.get("delta") {
                        let delta_type = delta
                            .get("type")
                            .and_then(|t| t.as_str())
                            .unwrap_or_default();
                        match delta_type {
                            "text_delta" => {
                                if let Some(text) = delta.get("text").and_then(|t| t.as_str())
                                    && !text.is_empty()
                                    && tx
                                        .send(Ok(StreamEvent::TextDelta(StreamChunk::delta(
                                            text.to_string(),
                                        ))))
                                        .await
                                        .is_err()
                                {
                                    return;
                                }
                            }
                            "input_json_delta" => {
                                if let Some(json) =
                                    delta.get("partial_json").and_then(|j| j.as_str())
                                {
                                    tool_input_json.push_str(json);
                                }
                            }
                            // TODO: handle "thinking_delta" events for streaming
                            // extended thinking content. Currently thinking blocks
                            // are only captured in non-streaming parse_native_response().
                            _ => {}
                        }
                    }
                }
                "content_block_stop" => {
                    if let Some(id) = tool_id.take() {
                        let name = tool_name.take().unwrap_or_default();
                        let input = std::mem::take(&mut tool_input_json);
                        let _ = tx
                            .send(Ok(StreamEvent::ToolCall(ProviderToolCall {
                                id,
                                name,
                                arguments: input,
                                extra_content: None,
                            })))
                            .await;
                    }
                }
                "message_delta" => {
                    let stop_reason = event
                        .get("delta")
                        .and_then(|d| d.get("stop_reason"))
                        .and_then(|s| s.as_str())
                        .unwrap_or("none");
                    if stop_reason != "none" {
                        saw_stop_reason = true;
                    }
                    // Anthropic's running-total: each `message_delta`
                    // supersedes the previous one, so we always overwrite.
                    let observed_output = event
                        .get("usage")
                        .and_then(|u| u.get("output_tokens"))
                        .and_then(|t| t.as_u64());
                    if let Some(v) = observed_output {
                        output_tokens = Some(v);
                    }
                    if stop_reason == "max_tokens" {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"output_tokens": observed_output})),
                            "response truncated: hit max_tokens limit. Increase provider_max_tokens in config."
                        );
                    } else {
                        ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"stop_reason": stop_reason, "output_tokens": observed_output})), "stream: message_delta");
                    }
                }
                "message_stop" => {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        "stream: message_stop"
                    );
                    if input_tokens.is_some()
                        || output_tokens.is_some()
                        || cached_input_tokens.is_some()
                        || cache_creation_input_tokens.is_some()
                    {
                        // Normalize to TokenUsage contract: `input_tokens` is
                        // the *total* prompt size. Anthropic reports three
                        // DISJOINT buckets per
                        // https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching:
                        //   total = cache_read + cache_creation + input_tokens
                        // where `input_tokens` from the API is "tokens after
                        // the last cache breakpoint", not the total.
                        let uncached = input_tokens.unwrap_or(0);
                        let cache_read = cached_input_tokens.unwrap_or(0);
                        let cache_create = cache_creation_input_tokens.unwrap_or(0);
                        let normalized_input = Some(
                            uncached
                                .saturating_add(cache_read)
                                .saturating_add(cache_create),
                        );
                        let _ = tx
                            .send(Ok(StreamEvent::Usage(TokenUsage {
                                input_tokens: normalized_input,
                                output_tokens,
                                cached_input_tokens,
                            })))
                            .await;
                    }
                    let _ = tx.send(Ok(StreamEvent::Final)).await;
                    return;
                }
                "error" => {
                    let msg = event
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("unknown streaming error");
                    let _ = tx
                        .send(Err(StreamError::ModelProvider(msg.to_string())))
                        .await;
                    return;
                }
                _ => {}
            }
        }

        crate::stream_guard::finish_sse_stream(tx, saw_stop_reason, "message_stop").await;
    }
}

#[async_trait]
impl ModelProvider for AnthropicModelProvider {
    fn default_temperature(&self) -> f64 {
        TEMPERATURE_DEFAULT
    }

    fn default_base_url(&self) -> Option<&str> {
        Some(BASE_URL)
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"missing": "credentials"})),
                "anthropic: no credentials configured"
            );
            anyhow::Error::msg(
                "Anthropic credentials not set. Set ANTHROPIC_API_KEY or ANTHROPIC_OAUTH_TOKEN (setup-token).",
            )
        })?;

        let system = system_prompt.map(|s| SystemPrompt::String(s.to_string()));
        let system = if Self::is_setup_token(credential) {
            Self::apply_oauth_system_prompt(system)
        } else {
            system
        };

        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"max_tokens": self.max_tokens, "model": model})),
            "API request"
        );
        let request = NativeChatRequest {
            model: model.to_string(),
            max_tokens: self.max_tokens,
            system,
            messages: vec![NativeMessage {
                role: "user".to_string(),
                content: vec![NativeContentOut::Text {
                    text: message.to_string(),
                    cache_control: None,
                }],
            }],
            temperature,
            tools: None,
            tool_choice: None,
            stream: None,
            thinking: None,
        };

        let mut request = self
            .http_client()
            .post(format!("{}/v1/messages", self.base_url))
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request);

        request = self.apply_auth(request, credential);

        let response = request.send().await?;

        if !response.status().is_success() {
            return Err(super::api_error("Anthropic", response).await);
        }

        let chat_response: NativeChatResponse = response.json().await?;
        let parsed = Self::parse_native_response(chat_response);
        parsed.text.ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                "anthropic: empty text in response"
            );
            anyhow::Error::msg("No response from Anthropic")
        })
    }

    async fn chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ProviderChatResponse> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"missing": "credentials"})),
                "anthropic: no credentials configured"
            );
            anyhow::Error::msg(
                "Anthropic credentials not set. Set ANTHROPIC_API_KEY or ANTHROPIC_OAUTH_TOKEN (setup-token).",
            )
        })?;

        let (system_prompt, mut messages) = Self::convert_messages(request.messages);

        // Auto-cache last message if conversation is long
        if Self::should_cache_conversation(request.messages) {
            Self::apply_cache_to_last_message(&mut messages);
        }

        // Check for tool_choice override from the agent loop (e.g. "any"
        // to force tool use for hardware requests).
        let tool_choice_override = zeroclaw_api::TOOL_CHOICE_OVERRIDE
            .try_with(Clone::clone)
            .ok()
            .flatten();
        let native_tools = Self::convert_tools(request.tools);
        let tools_count = native_tools.as_ref().map_or(0, Vec::len);
        let tool_choice = if native_tools.is_some() {
            tool_choice_override.map(|tc| serde_json::json!({ "type": tc }))
        } else {
            None
        };

        // For OAuth tokens, prepend Claude Code identity to system prompt
        let system_prompt = if Self::is_setup_token(credential) {
            Self::apply_oauth_system_prompt(system_prompt)
        } else {
            system_prompt
        };

        let (effective_temperature, thinking_config, effective_max_tokens) =
            self.resolve_thinking(request.thinking, temperature, model);

        if ::zeroclaw_log::debug_enabled() {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "provider": "anthropic",
                        "alias": &self.alias,
                        "request_api": "messages",
                        "model": model,
                        "stream": false,
                        "max_tokens": effective_max_tokens,
                        "tools_count": tools_count,
                        "tool_choice": tool_choice.as_ref().and_then(|value| value.get("type")).and_then(|value| value.as_str()),
                        "thinking_enabled": thinking_config.is_some(),
                    })),
                "anthropic provider request prepared"
            );
        }
        let native_request = NativeChatRequest {
            model: model.to_string(),
            max_tokens: effective_max_tokens,
            system: system_prompt,
            messages,
            temperature: effective_temperature,
            tools: native_tools,
            tool_choice,
            stream: None,
            thinking: thinking_config,
        };

        let req = self
            .http_client()
            .post(format!("{}/v1/messages", self.base_url))
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&native_request);

        let response = self.apply_auth(req, credential).send().await?;
        if !response.status().is_success() {
            return Err(super::api_error("Anthropic", response).await);
        }

        let native_response: NativeChatResponse = response.json().await?;
        Ok(Self::parse_native_response(native_response))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: true,
            vision: true,
            prompt_caching: true,
            extended_thinking: true,
        }
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ProviderChatResponse> {
        // Convert OpenAI-format tool JSON to ToolSpec so we can reuse the
        // existing `chat()` method which handles full message history,
        // system prompt extraction, caching, and Anthropic native formatting.
        let tool_specs: Vec<ToolSpec> = tools
            .iter()
            .filter_map(|t| {
                let func = t.get("function").or_else(|| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        "Skipping malformed tool definition (missing 'function' key)"
                    );
                    None
                })?;
                let name = func.get("name").and_then(|n| n.as_str()).or_else(|| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        "Skipping tool with missing or non-string 'name'"
                    );
                    None
                })?;
                Some(ToolSpec {
                    name: name.to_string(),
                    description: func
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string(),
                    parameters: std::sync::Arc::new(
                        func.get("parameters")
                            .cloned()
                            .unwrap_or(serde_json::json!({"type": "object"})),
                    ),
                })
            })
            .collect();

        let request = ProviderChatRequest {
            messages,
            tools: if tool_specs.is_empty() {
                None
            } else {
                Some(&tool_specs)
            },
            thinking: None,
        };
        self.chat(request, model, temperature).await
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        if let Some(credential) = self.credential.as_ref() {
            let mut request = self
                .http_client()
                .post(format!("{}/v1/messages", self.base_url))
                .header("anthropic-version", "2023-06-01");
            request = self.apply_auth(request, credential);
            // Send a minimal request; the goal is TLS + HTTP/2 setup, not a valid response.
            // Anthropic has no lightweight GET endpoint, so we accept any non-network error.
            let _ = request.send().await?;
        }
        Ok(())
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        // Anthropic's /v1/models requires a credential. Onboard pulls the
        // catalog from models.dev before the user has entered a key.
        crate::models_dev::list_models_for("anthropic").await
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_streaming_tool_events(&self) -> bool {
        true
    }

    fn stream_chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
        if !options.enabled {
            return stream::once(async { Ok(StreamEvent::Final) }).boxed();
        }

        let credential = match self.credential.as_ref() {
            Some(c) => c.clone(),
            None => {
                return stream::once(async {
                    Err(StreamError::ModelProvider(
                        "Anthropic credentials not set".to_string(),
                    ))
                })
                .boxed();
            }
        };

        let (system_prompt, mut messages) = Self::convert_messages(request.messages);
        if Self::should_cache_conversation(request.messages) {
            Self::apply_cache_to_last_message(&mut messages);
        }

        let tool_choice_override = zeroclaw_api::TOOL_CHOICE_OVERRIDE
            .try_with(Clone::clone)
            .ok()
            .flatten();
        let native_tools = Self::convert_tools(request.tools);
        let tools_count = native_tools.as_ref().map_or(0, Vec::len);
        let tool_choice = if native_tools.is_some() {
            tool_choice_override.map(|tc| serde_json::json!({ "type": tc }))
        } else {
            None
        };

        let system_prompt = if Self::is_setup_token(&credential) {
            Self::apply_oauth_system_prompt(system_prompt)
        } else {
            system_prompt
        };

        let (effective_temperature, thinking_config, effective_max_tokens) =
            self.resolve_thinking(request.thinking, temperature, model);

        // When native thinking is enabled, streamed `thinking_delta` /
        // `signature_delta` SSE events are not yet parsed into
        // `reasoning_content`, which means a tool-use turn could emit a
        // tool call without preserving the signed thinking block that
        // justified it — breaking Anthropic's signature round-trip. Fall
        // back to a non-streaming request so `parse_native_response` can
        // preserve the signed blocks, and synthesize a short stream from
        // the completed response. Full streaming thinking_delta
        // preservation is tracked as a follow-up.
        if thinking_config.is_some() {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "provider": "anthropic",
                        "alias": &self.alias,
                        "request_api": "messages",
                        "model": model,
                        "stream": false,
                        "tools_count": tools_count,
                        "tool_choice": tool_choice.as_ref().and_then(|value| value.get("type")).and_then(|value| value.as_str()),
                    })),
                "native thinking enabled; using non-streaming fallback to preserve signed thinking blocks"
            );
            let native_request = NativeChatRequest {
                model: model.to_string(),
                max_tokens: effective_max_tokens,
                system: system_prompt,
                messages,
                temperature: effective_temperature,
                tools: native_tools,
                tool_choice,
                stream: None,
                thinking: thinking_config,
            };
            // Serialize eagerly so the request body is owned and `'static`
            // across the async boundary.
            let body = serde_json::to_value(&native_request)
                .expect("NativeChatRequest should serialize to JSON");
            let client = self.http_client();
            let url = format!("{}/v1/messages", self.base_url);
            let is_oauth = Self::is_setup_token(&credential);

            return stream::once(async move {
                let mut req = client
                    .post(&url)
                    .header("anthropic-version", "2023-06-01")
                    .header("content-type", "application/json")
                    .json(&body);
                if is_oauth {
                    req = req
                        .header("Authorization", format!("Bearer {credential}"))
                        .header(
                            "anthropic-beta",
                            "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14",
                        )
                        .header("anthropic-dangerous-direct-browser-access", "true");
                } else {
                    req = req.header("x-api-key", &credential);
                }
                let response = req
                    .send()
                    .await
                    .map_err(|e| StreamError::Http(e.to_string()))?;
                if !response.status().is_success() {
                    let status = response.status();
                    let body = response
                        .text()
                        .await
                        .unwrap_or_else(|_| format!("HTTP error: {status}"));
                    return Err(StreamError::ModelProvider(format!("{status}: {body}")));
                }
                let parsed: NativeChatResponse = response
                    .json()
                    .await
                    .map_err(|e| StreamError::ModelProvider(format!("response decode: {e}")))?;
                Ok(Self::parse_native_response(parsed))
            })
            .flat_map(|result| match result {
                Ok(resp) => {
                    let mut events: Vec<StreamResult<StreamEvent>> = Vec::new();
                    // Emit signed thinking blocks first via `StreamChunk.reasoning`
                    // so the agent loop can accumulate them into
                    // `ChatResponse.reasoning_content` for multi-turn replay.
                    // Anthropic requires signed thinking blocks to precede
                    // tool-use blocks in conversation history.
                    if let Some(rc) = resp.reasoning_content {
                        events.push(Ok(StreamEvent::TextDelta(StreamChunk {
                            delta: String::new(),
                            reasoning: Some(rc),
                            is_final: false,
                            token_count: 0,
                        })));
                    }
                    if let Some(text) = resp.text.filter(|t| !t.is_empty()) {
                        events.push(Ok(StreamEvent::TextDelta(StreamChunk::delta(text))));
                    }
                    for tc in resp.tool_calls {
                        events.push(Ok(StreamEvent::ToolCall(tc)));
                    }
                    if let Some(usage) = resp.usage {
                        events.push(Ok(StreamEvent::Usage(usage)));
                    }
                    events.push(Ok(StreamEvent::Final));
                    stream::iter(events)
                }
                Err(e) => stream::iter(vec![Err(e)]),
            })
            .boxed();
        }

        if ::zeroclaw_log::debug_enabled() {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "provider": "anthropic",
                        "alias": &self.alias,
                        "request_api": "messages",
                        "model": model,
                        "stream": true,
                        "max_tokens": effective_max_tokens,
                        "tools_count": tools_count,
                        "tool_choice": tool_choice.as_ref().and_then(|value| value.get("type")).and_then(|value| value.as_str()),
                        "thinking_enabled": false,
                    })),
                "anthropic streaming provider request prepared"
            );
        }
        let native_request = NativeChatRequest {
            model: model.to_string(),
            max_tokens: effective_max_tokens,
            system: system_prompt,
            messages,
            temperature: effective_temperature,
            tools: native_tools,
            tool_choice,
            stream: Some(true),
            thinking: thinking_config,
        };

        let body = match Self::build_streaming_request(&native_request) {
            Ok(body) => body,
            Err(e) => {
                return stream::once(async move { Err(StreamError::ModelProvider(e.to_string())) })
                    .boxed();
            }
        };
        let client = self.http_client();
        let url = format!("{}/v1/messages", self.base_url);
        let is_oauth = Self::is_setup_token(&credential);

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(64);

        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Spawn)
                .with_category(::zeroclaw_log::EventCategory::Provider)
                .with_attrs(::serde_json::json!({
                    "idle_timeout_secs": SSE_IDLE_TIMEOUT.as_secs(),
                    "channel_capacity": 64,
                })),
            "stream: spawning detached Anthropic SSE parser task"
        );

        let parser_handle = ::zeroclaw_spawn::spawn!(async move {
            let mut req = client
                .post(&url)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body);

            if is_oauth {
                req = req
                    .header("Authorization", format!("Bearer {credential}"))
                    .header(
                        "anthropic-beta",
                        "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14",
                    )
                    .header("anthropic-dangerous-direct-browser-access", "true");
            } else {
                req = req.header("x-api-key", &credential);
            }

            let response = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx
                        .send(Err(StreamError::Http(super::format_error_chain(&e))))
                        .await;
                    return;
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let error = response
                    .text()
                    .await
                    .unwrap_or_else(|_| format!("HTTP error: {status}"));
                let _ = tx
                    .send(Err(StreamError::ModelProvider(format!(
                        "{status}: {error}"
                    ))))
                    .await;
                return;
            }

            Self::parse_anthropic_sse(response, &tx).await;
        });

        // The guard travels inside the unfold state so it is dropped at the
        // exact moment the consumer drops the stream — turning a turn cancel
        // (or normal completion) into an immediate parser-task abort instead
        // of a leaked socket that lingers until SSE_IDLE_TIMEOUT.
        let guard = AbortOnDrop::new(parser_handle.abort_handle());
        stream::unfold((rx, guard), |(mut rx, guard)| async move {
            rx.recv().await.map(|event| (event, (rx, guard)))
        })
        .boxed()
    }
}

impl ::zeroclaw_api::attribution::Attributable for AnthropicModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Anthropic,
            ),
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::anthropic_token::{AnthropicAuthKind, detect_auth_kind};

    /// Fake Anthropic SSE stream covering the message_start → content → delta
    /// → stop sequence with usage in both the start frame and the stop delta.
    /// Each `data:` line is one Anthropic event per the streaming spec.
    ///
    /// The usage frame includes all three disjoint input buckets
    /// (input_tokens=314 after-breakpoint, cache_read=42, cache_creation=100)
    /// so the test exercises the documented Anthropic formula:
    ///   total = cache_read + cache_creation + input_tokens
    fn fake_anthropic_sse() -> &'static [u8] {
        b"event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-sonnet-4-5\",\"usage\":{\"input_tokens\":314,\"cache_read_input_tokens\":42,\"cache_creation_input_tokens\":100}}}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":27}}\n\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\n"
    }

    #[tokio::test]
    async fn streaming_usage_emitted_before_final() {
        // The original #6001 live repro was Anthropic streaming; before this
        // PR the message_start / message_delta usage frames were only logged
        // at DEBUG and never surfaced as `StreamEvent::Usage`. Now they are.
        use std::io::Cursor;

        let bytes = fake_anthropic_sse();
        let reader = tokio::io::BufReader::new(Cursor::new(bytes));
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(64);
        AnthropicModelProvider::parse_anthropic_sse_from_reader(reader, &tx).await;

        let mut events = Vec::new();
        while let Ok(Some(ev)) =
            tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await
        {
            events.push(ev);
        }

        let states: Vec<&str> = events
            .iter()
            .map(|e| match e.as_ref() {
                Ok(StreamEvent::TextDelta(_)) => "text",
                Ok(StreamEvent::ToolCall(_)) => "tool_call",
                Ok(StreamEvent::PreExecutedToolCall { .. }) => "pre_tool_call",
                Ok(StreamEvent::PreExecutedToolResult { .. }) => "pre_tool_result",
                Ok(StreamEvent::Usage(_)) => "usage",
                Ok(StreamEvent::Final) => "final",
                Err(_) => "err",
            })
            .collect();

        // Required ordering: usage event must appear before Final so the
        // gateway accumulator can capture it within the same turn boundary.
        let usage_pos = states
            .iter()
            .position(|s| *s == "usage")
            .unwrap_or_else(|| panic!("expected Usage event in stream, got {states:?}"));
        let final_pos = states
            .iter()
            .position(|s| *s == "final")
            .unwrap_or_else(|| panic!("expected Final event in stream, got {states:?}"));
        assert!(
            usage_pos < final_pos,
            "Usage must come before Final, got {states:?}"
        );

        // The Usage payload must carry both input + output token counts plus
        // the cached-input prompt-cache reads from message_start.
        let usage = events
            .iter()
            .find_map(|e| match e.as_ref() {
                Ok(StreamEvent::Usage(u)) => Some(u.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            usage.input_tokens,
            Some(456),
            "input_tokens must be the total of all three Anthropic buckets \
             (after-breakpoint 314 + cache_read 42 + cache_creation 100) \
             per the documented prompt-caching formula"
        );
        assert_eq!(
            usage.output_tokens,
            Some(27),
            "output_tokens from message_delta usage frame"
        );
        assert_eq!(
            usage.cached_input_tokens,
            Some(42),
            "cache_read_input_tokens from message_start"
        );
    }

    /// A reader that yields one buffer of bytes, then parks forever — models
    /// an SSE connection that delivers `message_start` and then goes silent
    /// with the socket still open. Without the idle timeout this hangs the
    /// parser indefinitely.
    struct StallAfterReader {
        data: std::io::Cursor<Vec<u8>>,
        drained: bool,
    }

    impl tokio::io::AsyncRead for StallAfterReader {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            if self.drained {
                // Park without self-waking; the surrounding timeout's timer
                // provides the wake. Self-waking here would busy-spin under
                // paused time and starve the timer.
                return std::task::Poll::Pending;
            }
            let before = buf.filled().len();
            let inner = std::pin::Pin::new(&mut self.data);
            let res = inner.poll_read(cx, buf);
            // Once the seed buffer is exhausted, stall on the *next* read
            // rather than reporting EOF (0 bytes) — EOF would end the stream
            // cleanly and never exercise the idle timeout.
            if buf.filled().len() == before {
                self.drained = true;
                return std::task::Poll::Pending;
            }
            res
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_stream_times_out_instead_of_hanging() {
        // Repro: connection delivers message_start then goes silent. The
        // parser must surface a retryable StreamError rather than parking on
        // next_line() forever (the "stuck on working" hang).
        let start = b"event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude\",\"usage\":{\"input_tokens\":1}}}\n\n"
            .to_vec();
        let reader = tokio::io::BufReader::new(StallAfterReader {
            data: std::io::Cursor::new(start),
            drained: false,
        });
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(64);

        let parser = ::zeroclaw_spawn::spawn!(async move {
            AnthropicModelProvider::parse_anthropic_sse_from_reader(reader, &tx).await;
        });

        // Let the parser run until it parks on the stalled read before we
        // jump virtual time forward.
        tokio::task::yield_now().await;
        // Advance virtual time past the idle bound; the parser should wake,
        // emit an error, and return — closing the channel.
        tokio::time::advance(SSE_IDLE_TIMEOUT + std::time::Duration::from_secs(1)).await;

        let mut last_err = None;
        while let Some(ev) = rx.recv().await {
            if let Err(e) = ev {
                last_err = Some(e);
            }
        }
        parser.await.expect("parser task must finish, not hang");

        let err = last_err.expect("a StreamError must be emitted on stall");
        assert!(
            matches!(err, StreamError::Http(ref m) if m.contains("stalled")),
            "expected stalled-stream Http error, got: {err:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_guard_aborts_parser_without_idle_wait() {
        // The full-measure fix: dropping the consumer stream must abort the
        // detached parser immediately (turn cancel), not leak the socket until
        // SSE_IDLE_TIMEOUT. We model the stream's lifetime with AbortOnDrop and
        // assert the task is aborted the instant the guard drops.
        let start = b"event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude\",\"usage\":{\"input_tokens\":1}}}\n\n"
            .to_vec();
        let reader = tokio::io::BufReader::new(StallAfterReader {
            data: std::io::Cursor::new(start),
            drained: false,
        });
        let (tx, _rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(64);

        let handle = ::zeroclaw_spawn::spawn!(async move {
            AnthropicModelProvider::parse_anthropic_sse_from_reader(reader, &tx).await;
        });
        let probe = handle.abort_handle();
        let guard = AbortOnDrop::new(handle.abort_handle());

        // Let the parser park on the stalled read.
        tokio::task::yield_now().await;
        assert!(
            !probe.is_finished(),
            "parser must still be running (parked on the stalled read) before drop"
        );

        // Dropping the guard must abort the parser — no SSE_IDLE_TIMEOUT wait.
        drop(guard);
        tokio::task::yield_now().await;
        assert!(
            probe.is_finished(),
            "guard drop must abort the parser task immediately, not wait out the idle timeout"
        );
    }

    #[tokio::test]
    async fn eof_before_message_stop_surfaces_error_not_final() {
        // Live repro (trace aaf558a6): the SSE socket closed mid-response
        // after tool-result submission; the parser fell through to Final and
        // the turn ended as an empty "final response" with no explanation.
        // EOF without message_stop (or a stop_reason) is a truncated
        // response and must surface a retryable error.
        use std::io::Cursor;

        let bytes = b"event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude\",\"usage\":{\"input_tokens\":10}}}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n";
        let reader = tokio::io::BufReader::new(Cursor::new(bytes.as_slice()));
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(64);
        AnthropicModelProvider::parse_anthropic_sse_from_reader(reader, &tx).await;

        let mut saw_final = false;
        let mut last_err = None;
        while let Ok(Some(ev)) =
            tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await
        {
            match ev {
                Ok(StreamEvent::Final) => saw_final = true,
                Err(e) => last_err = Some(e),
                Ok(_) => {}
            }
        }
        assert!(!saw_final, "truncated stream must not emit Final");
        let err = last_err.expect("truncated stream must emit a StreamError");
        assert!(
            matches!(err, StreamError::Http(ref m) if m.contains("truncated")),
            "expected truncation error, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn streaming_usage_omitted_when_provider_does_not_send_usage() {
        // Backward-compat: a stream that never emits a usage frame must not
        // synthesize a zero-valued Usage event. Consumers should treat
        // absence as "usage unavailable" rather than "usage was zero."
        use std::io::Cursor;

        let bytes = b"event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude\"}}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\n";
        let reader = tokio::io::BufReader::new(Cursor::new(bytes.as_slice()));
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(64);
        AnthropicModelProvider::parse_anthropic_sse_from_reader(reader, &tx).await;

        let mut saw_usage = false;
        while let Ok(Some(ev)) =
            tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await
        {
            if matches!(ev, Ok(StreamEvent::Usage(_))) {
                saw_usage = true;
            }
        }
        assert!(
            !saw_usage,
            "must not emit Usage when provider sent no usage frames"
        );
    }

    #[test]
    fn creates_with_key() {
        let p = AnthropicModelProvider::new("test", Some("anthropic-test-credential"));
        assert!(p.credential.is_some());
        assert_eq!(p.credential.as_deref(), Some("anthropic-test-credential"));
        assert_eq!(p.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn creates_without_key() {
        let p = AnthropicModelProvider::new("test", None);
        assert!(p.credential.is_none());
        assert_eq!(p.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn creates_with_empty_key() {
        let p = AnthropicModelProvider::new("test", Some(""));
        assert!(p.credential.is_none());
    }

    #[test]
    fn creates_with_whitespace_key() {
        let p = AnthropicModelProvider::new("test", Some("  anthropic-test-credential  "));
        assert!(p.credential.is_some());
        assert_eq!(p.credential.as_deref(), Some("anthropic-test-credential"));
    }

    #[test]
    fn creates_with_custom_base_url() {
        let p = AnthropicModelProvider::with_base_url(
            "test",
            Some("anthropic-credential"),
            Some("https://api.example.com"),
        );
        assert_eq!(p.base_url, "https://api.example.com");
        assert_eq!(p.credential.as_deref(), Some("anthropic-credential"));
    }

    #[test]
    fn custom_base_url_trims_trailing_slash() {
        let p =
            AnthropicModelProvider::with_base_url("test", None, Some("https://api.example.com/"));
        assert_eq!(p.base_url, "https://api.example.com");
    }

    #[test]
    fn no_base_url_uses_published_endpoint() {
        let p = AnthropicModelProvider::with_base_url("test", None, None);
        assert_eq!(p.base_url, "https://api.anthropic.com");
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        let p = AnthropicModelProvider::new("test", None);
        let result = p
            .chat_with_system(None, "hello", "claude-3-opus", Some(0.7))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("credentials not set"),
            "Expected key error, got: {err}"
        );
    }

    #[test]
    fn setup_token_detection_works() {
        assert!(AnthropicModelProvider::is_setup_token(
            "sk-ant-oat01-abcdef"
        ));
        assert!(!AnthropicModelProvider::is_setup_token("sk-ant-api-key"));
    }

    #[test]
    fn apply_auth_uses_bearer_and_beta_for_setup_tokens() {
        let model_provider = AnthropicModelProvider::new("test", None);
        let request = model_provider
            .apply_auth(
                model_provider
                    .http_client()
                    .get("https://api.anthropic.com/v1/models"),
                "sk-ant-oat01-test-token",
            )
            .build()
            .expect("request should build");

        assert_eq!(
            request
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok()),
            Some("Bearer sk-ant-oat01-test-token")
        );
        assert_eq!(
            request
                .headers()
                .get("anthropic-beta")
                .and_then(|v| v.to_str().ok()),
            Some("claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14")
        );
        assert_eq!(
            request
                .headers()
                .get("anthropic-dangerous-direct-browser-access")
                .and_then(|v| v.to_str().ok()),
            Some("true")
        );
        assert!(request.headers().get("x-api-key").is_none());
    }

    #[test]
    fn apply_auth_uses_x_api_key_for_regular_tokens() {
        let model_provider = AnthropicModelProvider::new("test", None);
        let request = model_provider
            .apply_auth(
                model_provider
                    .http_client()
                    .get("https://api.anthropic.com/v1/models"),
                "sk-ant-api-key",
            )
            .build()
            .expect("request should build");

        assert_eq!(
            request
                .headers()
                .get("x-api-key")
                .and_then(|v| v.to_str().ok()),
            Some("sk-ant-api-key")
        );
        assert!(request.headers().get("authorization").is_none());
        assert!(request.headers().get("anthropic-beta").is_none());
    }

    #[tokio::test]
    async fn chat_with_system_fails_without_key() {
        let p = AnthropicModelProvider::new("test", None);
        let result = p
            .chat_with_system(
                Some("You are ZeroClaw"),
                "hello",
                "claude-3-opus",
                Some(0.7),
            )
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn chat_request_serializes_without_system() {
        let req = ChatRequest {
            model: "claude-3-opus".to_string(),
            max_tokens: 4096,
            system: None,
            messages: vec![Message {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            temperature: Some(0.7),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            !json.contains("system"),
            "system field should be skipped when None"
        );
        assert!(json.contains("claude-3-opus"));
        assert!(json.contains("hello"));
    }

    #[test]
    fn chat_request_serializes_with_system() {
        let req = ChatRequest {
            model: "claude-3-opus".to_string(),
            max_tokens: 4096,
            system: Some("You are ZeroClaw".to_string()),
            messages: vec![Message {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            temperature: Some(0.7),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"system\":\"You are ZeroClaw\""));
    }

    #[test]
    fn chat_response_deserializes() {
        let json = r#"{"content":[{"type":"text","text":"Hello there!"}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content.len(), 1);
        assert_eq!(resp.content[0].kind, "text");
        assert_eq!(resp.content[0].text.as_deref(), Some("Hello there!"));
    }

    #[test]
    fn chat_response_empty_content() {
        let json = r#"{"content":[]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.content.is_empty());
    }

    #[test]
    fn chat_response_multiple_blocks() {
        let json =
            r#"{"content":[{"type":"text","text":"First"},{"type":"text","text":"Second"}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content.len(), 2);
        assert_eq!(resp.content[0].text.as_deref(), Some("First"));
        assert_eq!(resp.content[1].text.as_deref(), Some("Second"));
    }

    #[test]
    fn temperature_range_serializes() {
        for temp in [0.0, 0.5, 1.0, 2.0] {
            let req = ChatRequest {
                model: "claude-3-opus".to_string(),
                max_tokens: 4096,
                system: None,
                messages: vec![],
                temperature: Some(temp),
            };
            let json = serde_json::to_string(&req).unwrap();
            assert!(json.contains(&format!("{temp}")));
        }
    }

    #[test]
    fn anthropic_model_supports_native_thinking_excludes_opus_4_7() {
        // Opus 4.7 only supports adaptive thinking; fixed-budget returns 400.
        assert!(!anthropic_model_supports_native_thinking("claude-opus-4-7"));
        assert!(!anthropic_model_supports_native_thinking(
            "claude-opus-4-7-20260101"
        ));
    }

    #[test]
    fn anthropic_model_supports_native_thinking_allows_other_models() {
        assert!(anthropic_model_supports_native_thinking("claude-opus-4-6"));
        assert!(anthropic_model_supports_native_thinking(
            "claude-sonnet-4-6"
        ));
        assert!(anthropic_model_supports_native_thinking("claude-haiku-4-5"));
    }

    #[test]
    fn resolve_thinking_drops_native_for_opus_4_7() {
        let provider = AnthropicModelProvider::new("test", Some("test-key"));
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 10_000,
        };
        let (temp, config, max_tokens) =
            provider.resolve_thinking(Some(params), Some(0.7_f64), "claude-opus-4-7");
        assert!(
            config.is_none(),
            "native thinking should be gated off for opus-4-7"
        );
        // Caller-supplied temperature is preserved (so per-model omit guard
        // can still take effect downstream).
        assert!((temp.unwrap() - 0.7_f64).abs() < f64::EPSILON);
        assert_eq!(max_tokens, provider.max_tokens);
    }

    #[test]
    fn resolve_thinking_keeps_native_for_supported_models() {
        let provider = AnthropicModelProvider::new("test", Some("test-key"));
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 10_000,
        };
        let (temp, config, _) =
            provider.resolve_thinking(Some(params), Some(0.7_f64), "claude-sonnet-4-6");
        assert!(
            config.is_some(),
            "native thinking should activate on supported models"
        );
        // Forced to 1.0 per Anthropic native-thinking contract.
        assert!((temp.unwrap() - 1.0_f64).abs() < f64::EPSILON);
    }

    #[test]
    fn native_chat_request_serializes_without_temperature_when_none() {
        let req = NativeChatRequest {
            model: "claude-opus-4-7".to_string(),
            max_tokens: 4096,
            system: None,
            messages: vec![],
            temperature: None,
            tools: None,
            tool_choice: None,
            stream: None,
            thinking: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("max_tokens"));
        assert!(
            !json.contains("temperature"),
            "expected temperature to be omitted, got: {json}"
        );
    }

    #[test]
    fn native_chat_request_serializes_with_temperature_when_some() {
        let req = NativeChatRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 4096,
            system: None,
            messages: vec![],
            temperature: Some(0.7),
            tools: None,
            tool_choice: None,
            stream: None,
            thinking: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            json.contains("\"temperature\":0.7"),
            "expected temperature to be present, got: {json}"
        );
    }

    #[test]
    fn detects_auth_from_jwt_shape() {
        let kind = detect_auth_kind("a.b.c", None);
        assert_eq!(kind, AnthropicAuthKind::Authorization);
    }

    #[test]
    fn cache_control_serializes_correctly() {
        let cache = CacheControl::ephemeral();
        let json = serde_json::to_string(&cache).unwrap();
        assert_eq!(json, r#"{"type":"ephemeral"}"#);
    }

    #[test]
    fn system_prompt_string_variant_serializes() {
        let prompt = SystemPrompt::String("You are a helpful assistant".to_string());
        let json = serde_json::to_string(&prompt).unwrap();
        assert_eq!(json, r#""You are a helpful assistant""#);
    }

    #[test]
    fn system_prompt_blocks_variant_serializes() {
        let prompt = SystemPrompt::Blocks(vec![SystemBlock {
            block_type: "text".to_string(),
            text: "You are a helpful assistant".to_string(),
            cache_control: Some(CacheControl::ephemeral()),
        }]);
        let json = serde_json::to_string(&prompt).unwrap();
        assert!(json.contains(r#""type":"text""#));
        assert!(json.contains("You are a helpful assistant"));
        assert!(json.contains(r#""type":"ephemeral""#));
    }

    #[test]
    fn system_prompt_blocks_without_cache_control() {
        let prompt = SystemPrompt::Blocks(vec![SystemBlock {
            block_type: "text".to_string(),
            text: "Short prompt".to_string(),
            cache_control: None,
        }]);
        let json = serde_json::to_string(&prompt).unwrap();
        assert!(json.contains("Short prompt"));
        assert!(!json.contains("cache_control"));
    }

    #[test]
    fn native_content_text_without_cache_control() {
        let content = NativeContentOut::Text {
            text: "Hello".to_string(),
            cache_control: None,
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains(r#""type":"text""#));
        assert!(json.contains("Hello"));
        assert!(!json.contains("cache_control"));
    }

    #[test]
    fn native_content_text_with_cache_control() {
        let content = NativeContentOut::Text {
            text: "Hello".to_string(),
            cache_control: Some(CacheControl::ephemeral()),
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains(r#""type":"text""#));
        assert!(json.contains("Hello"));
        assert!(json.contains(r#""cache_control":{"type":"ephemeral"}"#));
    }

    #[test]
    fn native_content_tool_use_without_cache_control() {
        let content = NativeContentOut::ToolUse {
            id: "tool_123".to_string(),
            name: "get_weather".to_string(),
            input: serde_json::json!({"location": "San Francisco"}),
            cache_control: None,
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains(r#""type":"tool_use""#));
        assert!(json.contains("tool_123"));
        assert!(json.contains("get_weather"));
        assert!(!json.contains("cache_control"));
    }

    #[test]
    fn native_content_tool_result_with_cache_control() {
        let content = NativeContentOut::ToolResult {
            tool_use_id: "tool_123".to_string(),
            content: "Result data".to_string(),
            cache_control: Some(CacheControl::ephemeral()),
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains(r#""type":"tool_result""#));
        assert!(json.contains("tool_123"));
        assert!(json.contains("Result data"));
        assert!(json.contains(r#""cache_control":{"type":"ephemeral"}"#));
    }

    #[test]
    fn native_tool_spec_without_cache_control() {
        let schema = serde_json::json!({"type": "object"});
        let tool = NativeToolSpec {
            name: "get_weather".to_string(),
            description: "Get weather info".to_string(),
            input_schema: schema.into(),
            cache_control: None,
        };
        let json = serde_json::to_string(&tool).unwrap();
        assert!(json.contains("get_weather"));
        assert!(!json.contains("cache_control"));
    }

    #[test]
    fn native_tool_spec_with_cache_control() {
        let schema = serde_json::json!({"type": "object"});
        let tool = NativeToolSpec {
            name: "get_weather".to_string(),
            description: "Get weather info".to_string(),
            input_schema: schema.into(),
            cache_control: Some(CacheControl::ephemeral()),
        };
        let json = serde_json::to_string(&tool).unwrap();
        assert!(json.contains("get_weather"));
        assert!(json.contains(r#""cache_control":{"type":"ephemeral"}"#));
    }

    #[test]
    fn should_cache_conversation_short() {
        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: "System prompt".to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
            },
        ];
        // Only 1 non-system message — should not cache
        assert!(!AnthropicModelProvider::should_cache_conversation(
            &messages
        ));
    }

    #[test]
    fn should_cache_conversation_long() {
        let mut messages = vec![ChatMessage {
            role: "system".to_string(),
            content: "System prompt".to_string(),
        }];
        // Add 3 non-system messages
        for i in 0..3 {
            messages.push(ChatMessage {
                role: if i % 2 == 0 { "user" } else { "assistant" }.to_string(),
                content: format!("Message {i}"),
            });
        }
        assert!(AnthropicModelProvider::should_cache_conversation(&messages));
    }

    #[test]
    fn should_cache_conversation_boundary() {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "Hello".to_string(),
        }];
        // Exactly 1 non-system message — should not cache
        assert!(!AnthropicModelProvider::should_cache_conversation(
            &messages
        ));

        // Add one more to cross boundary (>1)
        let messages = vec![
            ChatMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: "Hi".to_string(),
            },
        ];
        assert!(AnthropicModelProvider::should_cache_conversation(&messages));
    }

    #[test]
    fn apply_cache_to_last_message_text() {
        let mut messages = vec![NativeMessage {
            role: "user".to_string(),
            content: vec![NativeContentOut::Text {
                text: "Hello".to_string(),
                cache_control: None,
            }],
        }];

        AnthropicModelProvider::apply_cache_to_last_message(&mut messages);

        match &messages[0].content[0] {
            NativeContentOut::Text { cache_control, .. } => {
                assert!(cache_control.is_some());
            }
            _ => panic!("Expected Text variant"),
        }
    }

    #[test]
    fn apply_cache_to_last_message_tool_result() {
        let mut messages = vec![NativeMessage {
            role: "user".to_string(),
            content: vec![NativeContentOut::ToolResult {
                tool_use_id: "tool_123".to_string(),
                content: "Result".to_string(),
                cache_control: None,
            }],
        }];

        AnthropicModelProvider::apply_cache_to_last_message(&mut messages);

        match &messages[0].content[0] {
            NativeContentOut::ToolResult { cache_control, .. } => {
                assert!(cache_control.is_some());
            }
            _ => panic!("Expected ToolResult variant"),
        }
    }

    #[test]
    fn apply_cache_to_last_message_does_not_affect_tool_use() {
        let mut messages = vec![NativeMessage {
            role: "assistant".to_string(),
            content: vec![NativeContentOut::ToolUse {
                id: "tool_123".to_string(),
                name: "get_weather".to_string(),
                input: serde_json::json!({}),
                cache_control: None,
            }],
        }];

        AnthropicModelProvider::apply_cache_to_last_message(&mut messages);

        // ToolUse should not be affected
        match &messages[0].content[0] {
            NativeContentOut::ToolUse { cache_control, .. } => {
                assert!(cache_control.is_none());
            }
            _ => panic!("Expected ToolUse variant"),
        }
    }

    #[test]
    fn apply_cache_empty_messages() {
        let mut messages = vec![];
        AnthropicModelProvider::apply_cache_to_last_message(&mut messages);
        // Should not panic
        assert!(messages.is_empty());
    }

    #[test]
    fn convert_tools_adds_cache_to_last_tool() {
        let tools = vec![
            ToolSpec {
                name: "tool1".to_string(),
                description: "First tool".to_string(),
                parameters: serde_json::json!({"type": "object"}).into(),
            },
            ToolSpec {
                name: "tool2".to_string(),
                description: "Second tool".to_string(),
                parameters: serde_json::json!({"type": "object"}).into(),
            },
        ];

        let native_tools = AnthropicModelProvider::convert_tools(Some(&tools)).unwrap();

        assert_eq!(native_tools.len(), 2);
        assert!(native_tools[0].cache_control.is_none());
        assert!(native_tools[1].cache_control.is_some());
    }

    #[test]
    fn convert_tools_single_tool_gets_cache() {
        let tools = vec![ToolSpec {
            name: "tool1".to_string(),
            description: "Only tool".to_string(),
            parameters: serde_json::json!({"type": "object"}).into(),
        }];

        let native_tools = AnthropicModelProvider::convert_tools(Some(&tools)).unwrap();

        assert_eq!(native_tools.len(), 1);
        assert!(native_tools[0].cache_control.is_some());
    }

    #[test]
    fn convert_tools_cleans_ref_from_input_schema() {
        let tools = vec![ToolSpec {
            name: "query".to_string(),
            description: "Search with a ref".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "filter": {
                        "$ref": "#/$defs/FilterSpec"
                    }
                },
                "$defs": {
                    "FilterSpec": {
                        "type": "object",
                        "properties": {
                            "field": { "type": "string" }
                        }
                    }
                }
            })
            .into(),
        }];

        let native_tools = AnthropicModelProvider::convert_tools(Some(&tools)).unwrap();
        let schema = &native_tools[0].input_schema;

        let filter = &schema["properties"]["filter"];
        assert!(filter.get("$ref").is_none(), "$ref was not cleaned");
        assert_eq!(filter["type"], "object");
        assert_eq!(filter["properties"]["field"]["type"], "string");
        assert!(schema.get("$defs").is_none(), "$defs was not stripped");
    }

    #[test]
    fn convert_tools_cleans_definitions_from_input_schema() {
        let tools = vec![ToolSpec {
            name: "query".to_string(),
            description: "Search with a definitions ref".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "filter": {
                        "$ref": "#/definitions/FilterSpec"
                    }
                },
                "definitions": {
                    "FilterSpec": {
                        "type": "object",
                        "properties": {
                            "field": { "type": "string" }
                        }
                    }
                }
            })
            .into(),
        }];

        let native_tools = AnthropicModelProvider::convert_tools(Some(&tools)).unwrap();
        let schema = &native_tools[0].input_schema;

        let filter = &schema["properties"]["filter"];
        assert!(filter.get("$ref").is_none(), "$ref was not cleaned");
        assert_eq!(filter["type"], "object");
        assert!(
            schema.get("definitions").is_none(),
            "definitions was not stripped"
        );
    }

    #[test]
    fn convert_tools_empty_tools_returns_none() {
        let tools: Vec<ToolSpec> = vec![];
        let result = AnthropicModelProvider::convert_tools(Some(&tools));
        assert!(result.is_none());
    }

    #[test]
    fn convert_tools_none_returns_none() {
        let result: Option<Vec<NativeToolSpec>> = AnthropicModelProvider::convert_tools(None);
        assert!(result.is_none());
    }

    #[test]
    fn convert_messages_small_system_prompt_uses_blocks_with_cache() {
        let messages = vec![ChatMessage {
            role: "system".to_string(),
            content: "Short system prompt".to_string(),
        }];

        let (system_prompt, _) = AnthropicModelProvider::convert_messages(&messages);

        match system_prompt.unwrap() {
            SystemPrompt::Blocks(blocks) => {
                assert_eq!(blocks.len(), 1);
                assert_eq!(blocks[0].text, "Short system prompt");
                assert!(
                    blocks[0].cache_control.is_some(),
                    "Small system prompts should have cache_control"
                );
            }
            SystemPrompt::String(_) => {
                panic!("Expected Blocks variant with cache_control for small prompt")
            }
        }
    }

    #[test]
    fn convert_messages_large_system_prompt() {
        let large_content = "a".repeat(3073);
        let messages = vec![ChatMessage {
            role: "system".to_string(),
            content: large_content.clone(),
        }];

        let (system_prompt, _) = AnthropicModelProvider::convert_messages(&messages);

        match system_prompt.unwrap() {
            SystemPrompt::Blocks(blocks) => {
                assert_eq!(blocks.len(), 1);
                assert_eq!(blocks[0].text, large_content);
                assert!(blocks[0].cache_control.is_some());
            }
            SystemPrompt::String(_) => panic!("Expected Blocks variant for large prompt"),
        }
    }

    #[test]
    fn native_chat_request_with_blocks_system() {
        // System prompts now always use Blocks format with cache_control
        let req = NativeChatRequest {
            model: "claude-3-opus".to_string(),
            max_tokens: 4096,
            system: Some(SystemPrompt::Blocks(vec![SystemBlock {
                block_type: "text".to_string(),
                text: "System".to_string(),
                cache_control: Some(CacheControl::ephemeral()),
            }])),
            messages: vec![NativeMessage {
                role: "user".to_string(),
                content: vec![NativeContentOut::Text {
                    text: "Hello".to_string(),
                    cache_control: None,
                }],
            }],
            temperature: Some(0.7),
            tools: None,
            tool_choice: None,
            stream: None,
            thinking: None,
        };

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("System"));
        assert!(
            json.contains(r#""cache_control":{"type":"ephemeral"}"#),
            "System prompt should include cache_control"
        );
    }

    #[test]
    fn native_chat_request_omits_temperature_when_none() {
        let req = NativeChatRequest {
            model: "claude-opus-4-7".to_string(),
            max_tokens: 4096,
            system: None,
            messages: vec![NativeMessage {
                role: "user".to_string(),
                content: vec![NativeContentOut::Text {
                    text: "hi".to_string(),
                    cache_control: None,
                }],
            }],
            temperature: None,
            tools: None,
            tool_choice: None,
            stream: None,
            thinking: None,
        };

        let json = serde_json::to_string(&req).unwrap();
        assert!(
            !json.contains("temperature"),
            "temperature should be omitted when None; got: {json}"
        );
    }

    #[tokio::test]
    async fn warmup_without_key_is_noop() {
        let model_provider = AnthropicModelProvider::new("test", None);
        let result = model_provider.warmup().await;
        assert!(result.is_ok());
    }

    #[test]
    fn convert_messages_preserves_multi_turn_history() {
        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: "You are helpful.".to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: "gen a 2 sum in golang".to_string(),
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: "```go\nfunc twoSum(nums []int) {}\n```".to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: "what's meaning of make here?".to_string(),
            },
        ];

        let (system, native_msgs) = AnthropicModelProvider::convert_messages(&messages);

        // System prompt extracted
        assert!(system.is_some());
        // All 3 non-system messages preserved in order
        assert_eq!(native_msgs.len(), 3);
        assert_eq!(native_msgs[0].role, "user");
        assert_eq!(native_msgs[1].role, "assistant");
        assert_eq!(native_msgs[2].role, "user");
    }

    /// Integration test: spin up a mock Anthropic API server, call chat_with_tools
    /// with a multi-turn conversation + tools, and verify the request body contains
    /// ALL conversation turns and native tool definitions.
    #[tokio::test]
    async fn chat_with_tools_sends_full_history_and_native_tools() {
        use axum::{Json, Router, routing::post};
        use std::sync::{Arc, Mutex};
        use tokio::net::TcpListener;

        // Captured request body for assertion
        let captured: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let captured_clone = captured.clone();

        let app = Router::new().route(
            "/v1/messages",
            post(move |Json(body): Json<serde_json::Value>| {
                let cap = captured_clone.clone();
                async move {
                    *cap.lock().unwrap() = Some(body);
                    // Return a minimal valid Anthropic response
                    Json(serde_json::json!({
                        "id": "msg_test",
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "text", "text": "The make function creates a map."}],
                        "model": "claude-opus-4-6",
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 100, "output_tokens": 20}
                    }))
                }
            }),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_handle = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Create model_provider pointing at mock server
        let model_provider = AnthropicModelProvider {
            alias: "test".to_string(),
            credential: Some("test-key".to_string()),
            base_url: format!("http://{addr}"),
            max_tokens: 4096,
        };

        // Multi-turn conversation: system → user (Go code) → assistant (code response) → user (follow-up)
        let messages = vec![
            ChatMessage::system("You are a helpful assistant."),
            ChatMessage::user("gen a 2 sum in golang"),
            ChatMessage::assistant(
                "```go\nfunc twoSum(nums []int, target int) []int {\n    m := make(map[int]int)\n    for i, n := range nums {\n        if j, ok := m[target-n]; ok {\n            return []int{j, i}\n        }\n        m[n] = i\n    }\n    return nil\n}\n```",
            ),
            ChatMessage::user("what's meaning of make here?"),
        ];

        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "shell",
                "description": "Run a shell command",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {"type": "string"}
                    },
                    "required": ["command"]
                }
            }
        })];

        let result = model_provider
            .chat_with_tools(&messages, &tools, "claude-opus-4-6", Some(0.7))
            .await;
        assert!(result.is_ok(), "chat_with_tools failed: {:?}", result.err());

        let body = captured
            .lock()
            .unwrap()
            .take()
            .expect("No request captured");

        // Verify system prompt extracted to top-level field
        let system = &body["system"];
        assert!(
            system.to_string().contains("helpful assistant"),
            "System prompt missing: {system}"
        );

        // Verify ALL conversation turns present in messages array
        let msgs = body["messages"].as_array().expect("messages not an array");
        assert_eq!(
            msgs.len(),
            3,
            "Expected 3 messages (2 user + 1 assistant), got {}",
            msgs.len()
        );

        // Turn 1: user with Go request
        assert_eq!(msgs[0]["role"], "user");
        let turn1_text = msgs[0]["content"].to_string();
        assert!(
            turn1_text.contains("2 sum"),
            "Turn 1 missing Go request: {turn1_text}"
        );

        // Turn 2: assistant with Go code
        assert_eq!(msgs[1]["role"], "assistant");
        let turn2_text = msgs[1]["content"].to_string();
        assert!(
            turn2_text.contains("make(map[int]int)"),
            "Turn 2 missing Go code: {turn2_text}"
        );

        // Turn 3: user follow-up
        assert_eq!(msgs[2]["role"], "user");
        let turn3_text = msgs[2]["content"].to_string();
        assert!(
            turn3_text.contains("meaning of make"),
            "Turn 3 missing follow-up: {turn3_text}"
        );

        // Verify native tools are present
        let api_tools = body["tools"].as_array().expect("tools not an array");
        assert_eq!(api_tools.len(), 1);
        assert_eq!(api_tools[0]["name"], "shell");
        assert!(
            api_tools[0]["input_schema"].is_object(),
            "Missing input_schema"
        );

        server_handle.abort();
    }

    #[test]
    fn native_response_parses_usage() {
        let json = r#"{
            "content": [{"type": "text", "text": "Hello"}],
            "usage": {"input_tokens": 300, "output_tokens": 75}
        }"#;
        let resp: NativeChatResponse = serde_json::from_str(json).unwrap();
        let result = AnthropicModelProvider::parse_native_response(resp);
        let usage = result.usage.unwrap();
        assert_eq!(usage.input_tokens, Some(300));
        assert_eq!(usage.output_tokens, Some(75));
    }

    #[test]
    fn native_response_sums_all_three_anthropic_input_buckets() {
        // Per https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching:
        //   total_input = cache_read + cache_creation + input_tokens
        // where Anthropic's `input_tokens` is *only* the tokens after the
        // last cache breakpoint. The three buckets are disjoint.
        //
        // This is the most common live shape on a long Anthropic session:
        // huge cache_read, tiny input_tokens, occasional cache_creation as
        // the conversation grows past the previous breakpoint.
        let json = r#"{
            "content": [{"type": "text", "text": "ok"}],
            "usage": {
                "input_tokens": 1,
                "cache_read_input_tokens": 148539,
                "cache_creation_input_tokens": 4200,
                "output_tokens": 27
            }
        }"#;
        let resp: NativeChatResponse = serde_json::from_str(json).unwrap();
        let result = AnthropicModelProvider::parse_native_response(resp);
        let usage = result.usage.expect("usage should be Some");
        assert_eq!(
            usage.input_tokens,
            Some(152_740),
            "total = 1 (after-breakpoint) + 148539 (cache_read) + 4200 (cache_creation)"
        );
        assert_eq!(
            usage.cached_input_tokens,
            Some(148_539),
            "cached_input_tokens is the cache-read portion only \
             (the discount-billed subset of the total)"
        );
        assert_eq!(usage.output_tokens, Some(27));
    }

    #[test]
    fn native_response_parses_without_usage() {
        let json = r#"{"content": [{"type": "text", "text": "Hello"}]}"#;
        let resp: NativeChatResponse = serde_json::from_str(json).unwrap();
        let result = AnthropicModelProvider::parse_native_response(resp);
        assert!(result.usage.is_none());
    }

    #[test]
    fn native_response_preserves_thinking_text_byte_for_byte() {
        // Signatures on extended-thinking blocks are computed over the exact
        // bytes the model returned. Any mutation — including trim() — breaks
        // signature validation on replay in a multi-turn tool-use conversation.
        let json = r#"{
            "content": [
                {
                    "type": "thinking",
                    "thinking": "  \nStep 1: consider the request.\nStep 2: respond.\n  ",
                    "signature": "sig_abc123"
                },
                {"type": "text", "text": "ok"}
            ]
        }"#;
        let resp: NativeChatResponse = serde_json::from_str(json).unwrap();
        let result = AnthropicModelProvider::parse_native_response(resp);
        let reasoning = result.reasoning_content.expect("thinking preserved");
        let parsed: serde_json::Value = serde_json::from_str(&reasoning).unwrap();
        assert_eq!(
            parsed.get("thinking").and_then(|v| v.as_str()),
            Some("  \nStep 1: consider the request.\nStep 2: respond.\n  ")
        );
        assert_eq!(
            parsed.get("signature").and_then(|v| v.as_str()),
            Some("sig_abc123")
        );
    }

    #[test]
    fn native_response_drops_empty_thinking_blocks() {
        let json = r#"{
            "content": [
                {"type": "thinking", "thinking": "", "signature": "sig_xyz"},
                {"type": "text", "text": "hello"}
            ]
        }"#;
        let resp: NativeChatResponse = serde_json::from_str(json).unwrap();
        let result = AnthropicModelProvider::parse_native_response(resp);
        assert!(result.reasoning_content.is_none());
    }

    #[test]
    fn capabilities_returns_vision_and_native_tools() {
        let model_provider = AnthropicModelProvider::new("test", Some("test-key"));
        let caps = model_provider.capabilities();
        assert!(
            caps.native_tool_calling,
            "Anthropic should support native tool calling"
        );
        assert!(caps.vision, "Anthropic should support vision");
    }

    #[test]
    fn convert_messages_with_image_marker_data_uri() {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "Check this image: [IMAGE:data:image/jpeg;base64,/9j/4AAQ] What do you see?"
                .to_string(),
        }];

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);

        assert_eq!(native_msgs.len(), 1);
        assert_eq!(native_msgs[0].role, "user");
        // Should have 2 content blocks: image + text
        assert_eq!(native_msgs[0].content.len(), 2);

        // First block should be image
        match &native_msgs[0].content[0] {
            NativeContentOut::Image { source } => {
                assert_eq!(source.source_type, "base64");
                assert_eq!(source.media_type, "image/jpeg");
                assert_eq!(source.data, "/9j/4AAQ");
            }
            _ => panic!("Expected Image content block"),
        }

        // Second block should be text (parse_image_markers may leave extra spaces)
        match &native_msgs[0].content[1] {
            NativeContentOut::Text { text, .. } => {
                // The text may have extra spaces where the marker was removed
                assert!(
                    text.contains("Check this image:") && text.contains("What do you see?"),
                    "Expected text to contain 'Check this image:' and 'What do you see?', got: {}",
                    text
                );
            }
            _ => panic!("Expected Text content block"),
        }
    }

    #[test]
    fn convert_messages_with_only_image_marker() {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "[IMAGE:data:image/png;base64,iVBORw0KGgo]".to_string(),
        }];

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);

        assert_eq!(native_msgs.len(), 1);
        assert_eq!(native_msgs[0].content.len(), 2);

        // First block should be image
        match &native_msgs[0].content[0] {
            NativeContentOut::Image { source } => {
                assert_eq!(source.media_type, "image/png");
            }
            _ => panic!("Expected Image content block"),
        }

        // Second block should be placeholder text
        match &native_msgs[0].content[1] {
            NativeContentOut::Text { text, .. } => {
                assert_eq!(text, "[image]");
            }
            _ => panic!("Expected Text content block with [image] placeholder"),
        }
    }

    #[test]
    fn convert_messages_without_image_marker() {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "Hello, how are you?".to_string(),
        }];

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);

        assert_eq!(native_msgs.len(), 1);
        assert_eq!(native_msgs[0].content.len(), 1);

        match &native_msgs[0].content[0] {
            NativeContentOut::Text { text, .. } => {
                assert_eq!(text, "Hello, how are you?");
            }
            _ => panic!("Expected Text content block"),
        }
    }

    #[test]
    fn image_content_serializes_correctly() {
        let content = NativeContentOut::Image {
            source: ImageSource {
                source_type: "base64".to_string(),
                media_type: "image/jpeg".to_string(),
                data: "testdata".to_string(),
            },
        };
        let json = serde_json::to_string(&content).unwrap();
        // The outer "type" is the enum tag, inner "type" (source_type) is renamed
        assert!(json.contains(r#""type":"image""#), "JSON: {}", json);
        assert!(json.contains(r#""type":"base64""#), "JSON: {}", json); // source_type is serialized as "type"
        assert!(
            json.contains(r#""media_type":"image/jpeg""#),
            "JSON: {}",
            json
        );
        assert!(json.contains(r#""data":"testdata""#), "JSON: {}", json);
    }

    #[test]
    fn convert_messages_merges_consecutive_tool_results() {
        // Simulate a multi-tool-call turn: assistant with two tool_use blocks
        // followed by two separate tool result messages.
        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: "You are helpful.".to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: "Do two things.".to_string(),
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: serde_json::json!({
                    "content": "",
                    "tool_calls": [
                        {"id": "call_1", "name": "shell", "arguments": "{\"command\":\"ls\"}"},
                        {"id": "call_2", "name": "shell", "arguments": "{\"command\":\"pwd\"}"}
                    ]
                })
                .to_string(),
            },
            ChatMessage {
                role: "tool".to_string(),
                content: serde_json::json!({
                    "tool_call_id": "call_1",
                    "content": "file1.txt\nfile2.txt"
                })
                .to_string(),
            },
            ChatMessage {
                role: "tool".to_string(),
                content: serde_json::json!({
                    "tool_call_id": "call_2",
                    "content": "/home/user"
                })
                .to_string(),
            },
        ];

        let (system, native_msgs) = AnthropicModelProvider::convert_messages(&messages);

        assert!(system.is_some());
        // Should be: user, assistant, user (merged tool results)
        // NOT: user, assistant, user, user (which Anthropic rejects)
        assert_eq!(
            native_msgs.len(),
            3,
            "Expected 3 messages (user, assistant, merged tool results), got {}.\nRoles: {:?}",
            native_msgs.len(),
            native_msgs.iter().map(|m| &m.role).collect::<Vec<_>>()
        );
        assert_eq!(native_msgs[0].role, "user");
        assert_eq!(native_msgs[1].role, "assistant");
        assert_eq!(native_msgs[2].role, "user");
        // The merged user message should contain both tool results
        assert_eq!(
            native_msgs[2].content.len(),
            2,
            "Expected 2 tool_result blocks in merged message"
        );
    }

    #[test]
    fn convert_messages_backfills_orphaned_tool_use() {
        // A turn interrupted mid-flight: assistant emitted a tool_use but the
        // matching tool_result was never persisted, and a new user message
        // follows. Sending this raw is a hard 400. The converter must
        // synthesize a stub tool_result so the history stays well-formed.
        let messages = vec![
            ChatMessage {
                role: "user".to_string(),
                content: "Do a thing.".to_string(),
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: serde_json::json!({
                    "content": "",
                    "tool_calls": [
                        {"id": "orphan_1", "name": "shell", "arguments": "{\"command\":\"ls\"}"}
                    ]
                })
                .to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: "Actually, never mind.".to_string(),
            },
        ];

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);

        let assistant_idx = native_msgs
            .iter()
            .position(|m| m.role == "assistant")
            .expect("assistant message present");
        let next = native_msgs
            .get(assistant_idx + 1)
            .expect("a message must follow the tool_use");

        let has_stub = next.content.iter().any(|block| {
            matches!(
                block,
                NativeContentOut::ToolResult { tool_use_id, .. } if tool_use_id == "orphan_1"
            )
        });
        assert!(
            has_stub,
            "orphaned tool_use should be answered by a synthesized tool_result"
        );

        // The tool_result must lead its message (before sibling text).
        assert!(
            matches!(
                next.content.first(),
                Some(NativeContentOut::ToolResult { .. })
            ),
            "tool_result must precede any text in the user message"
        );
    }

    #[test]
    fn convert_messages_backfills_trailing_orphaned_tool_use() {
        // The interrupted tool_use is the very last thing in history with no
        // following message at all. A tool_result message must be appended.
        let messages = vec![
            ChatMessage {
                role: "user".to_string(),
                content: "Do a thing.".to_string(),
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: serde_json::json!({
                    "content": "",
                    "tool_calls": [
                        {"id": "trailing_1", "name": "shell", "arguments": "{}"}
                    ]
                })
                .to_string(),
            },
        ];

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);

        let last = native_msgs.last().expect("messages present");
        assert_eq!(last.role, "user");
        assert!(
            last.content.iter().any(|block| matches!(
                block,
                NativeContentOut::ToolResult { tool_use_id, .. } if tool_use_id == "trailing_1"
            )),
            "trailing orphaned tool_use should get an appended tool_result message"
        );
    }

    #[test]
    fn convert_messages_no_adjacent_same_role() {
        // Verify that convert_messages never produces adjacent messages with the
        // same role, regardless of input ordering.
        let messages = vec![
            ChatMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: serde_json::json!({
                    "content": "I'll run a command",
                    "tool_calls": [
                        {"id": "tc1", "name": "shell", "arguments": "{\"command\":\"echo hi\"}"}
                    ]
                })
                .to_string(),
            },
            ChatMessage {
                role: "tool".to_string(),
                content: serde_json::json!({
                    "tool_call_id": "tc1",
                    "content": "hi"
                })
                .to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: "Thanks!".to_string(),
            },
        ];

        let (_system, native_msgs) = AnthropicModelProvider::convert_messages(&messages);

        for window in native_msgs.windows(2) {
            assert_ne!(
                window[0].role, window[1].role,
                "Adjacent messages must not share the same role: found two '{}' messages in a row",
                window[0].role
            );
        }
    }
}
