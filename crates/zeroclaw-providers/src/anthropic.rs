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
/// Anthropic's documented per-image ceiling for the direct API: 10 MB
/// **base64-encoded**. Measured on the encoded payload length, unlike the
/// multimodal config's `max_image_size_mb`, which bounds decoded bytes. MB is
/// read as 1024 * 1024 here, the same way `max_image_size_mb` reads it, so the
/// two ceilings stay consistent with each other. Anthropic's separate
/// per-request budget (32 MB across all images) is not enforced here.
const MAX_ENCODED_IMAGE_PAYLOAD_BYTES: usize = 10 * 1024 * 1024;
/// Replaces a raw `data:<media type>;base64,<payload>` run that survived marker
/// parsing and would otherwise sit in a text position. See
/// [`AnthropicModelProvider::sweep_residual_image_data`].
///
/// Worded without "image" on purpose. The sweep matches any media type, because
/// any base64 blob in a text position is the token blowup it exists to stop, so
/// a note claiming an image was removed would be false for
/// `data:application/json;base64,…`. Like the omission note, this is prompt text
/// the model reads as fact.
const TRUNCATED_DATA_NOTE: &str = "[truncated inline data removed]";
/// Stand-in prose for a message whose only content is an image, so the message
/// never ends on an `image` block. See
/// [`AnthropicModelProvider::unpaired_tool_output_blocks`].
const IMAGE_ONLY_TEXT_PLACEHOLDER: &str = "[image]";
/// Prefix on tool output demoted to top-level blocks because an earlier block in
/// the same message already answered its `tool_use`. Without it the model reads
/// a tool's second answer as something the user typed. See
/// [`AnthropicModelProvider::demoted_tool_result_blocks`].
const DEMOTED_TOOL_RESULT_PREFIX: &str = "[duplicate result for tool call";
/// Narrowest line width the residual sweep will read as line-wrapped base64. No
/// encoder wraps this narrow — MIME uses 76, PEM and `base64` use 64, Ruby uses
/// 60 — so below it a column of equal-length short tokens is far likelier than a
/// wrapped payload. See [`AnthropicModelProvider::residual_payload_end`].
const WRAPPED_BASE64_WIDTH_MIN: usize = 16;

use crate::stream_guard::AbortOnDrop;
use std::borrow::Cow;

/// Maximum silence between body reads for Anthropic SSE streams.
const STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

pub struct AnthropicModelProvider {
    /// `[providers.models.anthropic.<alias>]` config-key alias.
    alias: String,
    credential: Option<String>,
    base_url: String,
    max_tokens: u32,
    timeout_secs: u64,
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

fn anthropic_model_supports_native_thinking(model: &str) -> bool {
    !model.contains("claude-opus-4-7")
}

/// Characters legal between `data:` and `;base64,` in a data URI header: the
/// media type plus any parameters. Whitespace, commas and brackets are excluded
/// so a stray `data:` in prose cannot claim a `;base64,` further down the string.
fn is_data_uri_header_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '/' | '+' | '-' | '.' | '_' | ';' | '=')
}

/// The standard base64 alphabet plus its padding character.
fn is_base64_payload_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '=')
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
        content: ToolResultContent,
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

/// `tool_result.content` accepts either a plain string or a list of nested
/// blocks. The string shape is **untagged**, so an image-free tool result still
/// serializes as a bare JSON string for `content` — byte-identical to what this
/// adapter sent before nested blocks existed.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ToolResultContent {
    Text(String),
    Blocks(Vec<ToolResultBlock>),
}

/// A block nested inside a `tool_result`. Anthropic also accepts `document` and
/// `search_result` here, but this adapter can only build `text` and `image`.
/// Keeping this separate from [`NativeContentOut`] makes a `tool_use` or a
/// nested `tool_result` — both of which the API rejects — unrepresentable.
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum ToolResultBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: ImageSource },
}

/// The tool-result envelope this crate's runtime writes for a native tool call:
/// `{"tool_call_id": …, "content": "…"}`. Parsed, never serialized.
struct ToolResultEnvelope {
    /// `None` when `tool_call_id` is present but not a string — a shape the
    /// current turn engine does not emit but restored or externally supplied
    /// history can. The caller then tries to recover the id from the assistant
    /// turn this message follows.
    tool_use_id: Option<String>,
    /// The tool's own output, with the envelope scaffolding removed.
    content: String,
}

#[derive(Debug, Serialize)]
struct NativeToolSpec {
    name: String,
    description: String,
    /// `Arc`-shared with the tool registry's stored schema when no cleaning
    /// is required — serialized transparently, deep-cloned only for schemas
    /// the Anthropic cleaner actually rewrites
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

/// Typed builder for [`AnthropicModelProvider`].
///
/// `alias` is the only positional argument. Everything else has a
/// sensible default: the base URL falls back to Anthropic's published
/// endpoint, no credential leaves the provider unauthenticated (fine
/// for local mocks), and token/timeout limits use the workspace baselines.
#[must_use]
pub struct AnthropicBuilder {
    alias: String,
    credential: Option<String>,
    base_url: Option<String>,
    max_tokens: Option<u32>,
    timeout_secs: Option<u64>,
}

impl AnthropicBuilder {
    /// Explicit API credential. Whitespace-only inputs are normalized
    /// to `None` so a stray `Some("   ")` from config cannot produce a
    /// bogus `Bearer    ` header.
    pub fn credential(mut self, credential: Option<&str>) -> Self {
        self.credential = credential
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(ToString::to_string);
        self
    }

    /// Override the API endpoint. Trailing slashes are stripped so
    /// callers need not care whether config supplied them.
    pub fn base_url(mut self, base_url: &str) -> Self {
        self.base_url = Some(base_url.trim_end_matches('/').to_string());
        self
    }

    /// Override the maximum output tokens for API requests. Defaults to
    /// [`zeroclaw_api::model_provider::BASELINE_MAX_TOKENS`] when unset.
    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Override the HTTP request timeout for LLM API calls. Defaults to
    /// [`zeroclaw_api::model_provider::BASELINE_TIMEOUT_SECS`] when unset.
    pub fn timeout_secs(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = Some(timeout_secs);
        self
    }

    pub fn build(self) -> AnthropicModelProvider {
        AnthropicModelProvider {
            alias: self.alias,
            credential: self.credential,
            base_url: self.base_url.unwrap_or_else(|| BASE_URL.to_string()),
            max_tokens: self
                .max_tokens
                .unwrap_or(zeroclaw_api::model_provider::BASELINE_MAX_TOKENS),
            timeout_secs: self
                .timeout_secs
                .unwrap_or(zeroclaw_api::model_provider::BASELINE_TIMEOUT_SECS),
        }
    }
}

impl AnthropicModelProvider {
    /// Entry point. Only `alias` is required; every other field is set
    /// via a labelled chain method on the returned [`AnthropicBuilder`].
    pub fn builder(alias: &str) -> AnthropicBuilder {
        AnthropicBuilder {
            alias: alias.to_string(),
            credential: None,
            base_url: None,
            max_tokens: None,
            timeout_secs: None,
        }
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

    /// Note appended to text when an image reference could not be sent.
    ///
    /// This is prompt text the model reads as fact, not user-facing UI text, so
    /// it stays an English literal rather than going through the Fluent
    /// catalogue.
    fn image_omission_note(count: usize) -> String {
        format!("[{count} image(s) omitted: unsupported or oversized image reference]")
    }

    /// Builds `tool_result.content` from tool-result text, turning image
    /// markers into nested `image` blocks.
    ///
    /// Multimodal preparation normalizes `[IMAGE:<path>]` to a `data:` URI
    /// whenever the provider reports `vision`. Before nested blocks existed the
    /// payload was serialized into a text position and billed as prose — tens of
    /// thousands of tokens the model reads as gibberish rather than as an image.
    ///
    /// References that fail the shared structural check are dropped and counted
    /// in an omission note instead. With no markers at all the original string
    /// is returned unchanged, so the common path is byte-identical to before.
    ///
    /// Block order is text first, then images, matching Anthropic's own
    /// documented example. (The user-message arm emits images first and text
    /// after; the two arms differ, and the ordering rule Anthropic enforces is
    /// about `tool_result` blocks relative to other blocks in a message, not
    /// about text relative to image inside a block list.)
    fn tool_result_content(content: &str) -> ToolResultContent {
        let (cleaned, refs) = crate::multimodal::parse_image_markers(content);
        if refs.is_empty() {
            // The early return still sweeps. An unterminated marker yields zero
            // references and copies its payload verbatim into the cleaned text,
            // so returning here without sweeping would leave raw base64 in a
            // text position on exactly the path that has no references.
            return ToolResultContent::Text(Self::sweep_residual_image_data(content).into_owned());
        }

        let (sources, omitted) = Self::deliverable_image_sources(&refs);
        // Unloadable placeholders stay in `cleaned` as prose and never reach
        // `refs`, so the count only covers references that were recognised and
        // still could not be sent.
        let text = Self::text_with_omission_note(&cleaned, omitted);

        if sources.is_empty() {
            return ToolResultContent::Text(text);
        }

        let mut blocks = Vec::with_capacity(sources.len() + 1);
        if !text.is_empty() {
            blocks.push(ToolResultBlock::Text { text });
        }
        blocks.extend(
            sources
                .into_iter()
                .map(|source| ToolResultBlock::Image { source }),
        );
        ToolResultContent::Blocks(blocks)
    }

    /// Turns image references into deliverable [`ImageSource`]s, returning how
    /// many were rejected by the shared structural check.
    fn deliverable_image_sources(refs: &[String]) -> (Vec<ImageSource>, usize) {
        let mut sources = Vec::new();
        let mut omitted = 0usize;
        for reference in refs {
            match crate::multimodal::split_base64_image_data_uri(
                reference,
                MAX_ENCODED_IMAGE_PAYLOAD_BYTES,
            ) {
                Ok((media_type, payload)) => sources.push(ImageSource {
                    source_type: "base64".to_string(),
                    media_type: media_type.to_ascii_lowercase(),
                    data: payload.to_string(),
                }),
                Err(_) => omitted += 1,
            }
        }
        (sources, omitted)
    }

    /// Sweeps residual raw base64 out of marker-cleaned prose and appends the
    /// omission note when references were rejected.
    fn text_with_omission_note(cleaned: &str, omitted: usize) -> String {
        let swept = Self::sweep_residual_image_data(cleaned);
        if omitted == 0 {
            return swept.into_owned();
        }
        let note = Self::image_omission_note(omitted);
        if swept.is_empty() {
            note
        } else {
            format!("{swept}\n\n{note}")
        }
    }

    /// Replaces every residual `data:<media type>;base64,<payload>` run in
    /// `text` with [`TRUNCATED_DATA_NOTE`].
    ///
    /// `crate::multimodal::parse_image_markers` does not extract an
    /// **unterminated** marker: with no closing `]` it copies the rest of the
    /// string verbatim into the cleaned text and returns no reference at all. A
    /// history truncated mid-marker would otherwise still put raw base64 in a
    /// text position, which is what this adapter must not do.
    ///
    /// The scan starts at the `data:` prefix, never at a bare payload — a
    /// payload whose header was already truncated away is indistinguishable from
    /// prose and is left alone. A run ends at the first character outside the
    /// base64 alphabet and its padding, except that it continues into the next
    /// line when the lines are uniformly wide (see
    /// [`Self::residual_payload_end`]). The whole run including its header is
    /// replaced.
    ///
    /// A swept run is deliberately **not** added to the omission count: the
    /// count means "references that were recognised and could not be sent", and
    /// a truncated marker was never a reference. Keeping it out of the count is
    /// what stops the sweep from double-reporting.
    ///
    /// Prose that legitimately quotes a data URI is rewritten too. That is
    /// accepted: a documentation-style example in a tool result is far rarer
    /// than a truncated screenshot, and the replacement says what happened.
    ///
    /// **What this does not cover**, stated so a reader does not credit it with
    /// more than it does:
    ///
    /// - A header whose media type holds a non-ASCII or otherwise implausible
    ///   character, such as `data:imagé/png;base64,…`. Such a header cannot come
    ///   from this crate's preparation code, and loosening the header rule would
    ///   let any `data:` in prose claim a `;base64,` further down the string.
    /// - Assistant message text. This runs on the two tool-result arms and the
    ///   user arm — the three arms built from `parse_image_markers` output.
    ///   Assistant content is copied to the wire verbatim, so a data URI the
    ///   model itself wrote is left as the model wrote it.
    /// - The last, shorter line of a wrapped payload, and the tail of a run that
    ///   is not uniformly wrapped. See [`Self::residual_payload_end`].
    ///
    /// The whole pass is linear in the length of `text`, and deliberately so:
    /// tool output is untrusted, and an earlier version that restarted a search
    /// for `;base64,` from every rejected `data:` cost minutes of CPU on a
    /// one-megabyte input that repeats `data:`.
    fn sweep_residual_image_data(text: &str) -> Cow<'_, str> {
        const SCHEME: &str = "data:";

        let mut out: Option<String> = None;
        // Everything before `copied` is already in `out`; `scan` is where the
        // next search for `data:` starts. They differ whenever a `data:` was
        // examined and left alone.
        let mut copied = 0usize;
        let mut scan = 0usize;

        while let Some(relative) = text[scan..].find(SCHEME) {
            let start = scan + relative;
            let header_start = start + SCHEME.len();
            // Walk the header forward rather than searching ahead for
            // `;base64,`: a real header holds only media-type and parameter
            // characters, so it either ends at `;base64,` or this `data:` is
            // prose. Walking keeps the pass linear.
            let header_end =
                header_start + Self::run_len(&text[header_start..], is_data_uri_header_char);
            // `base64` may sit anywhere in the parameter list, which is what
            // `crate::multimodal::split_base64_image_data_uri` accepts. Requiring
            // it last would leave `data:image/png;base64;charset=x,<payload>`
            // unswept while the same header is delivered as an image elsewhere.
            let mut header_parts = text[header_start..header_end].split(';');
            let plausible_header = header_parts
                .next()
                .is_some_and(|media_type| !media_type.is_empty())
                && header_parts.any(|parameter| parameter == "base64")
                && text[header_end..].starts_with(',');
            if !plausible_header {
                // Resume at the character after `data:`, not at `header_end`: the
                // four letters of `data` are header-legal, so a second `data:`
                // can start inside the header run just walked and jumping past it
                // would let `data:data:image/png;base64,<payload>` through
                // untouched. `data:` has no proper prefix that is also a suffix,
                // so occurrences are at least five bytes apart and resuming here
                // skips nothing.
                //
                // This stays linear. A header walk can only reach the `:` of the
                // next `data:`, so the walks starting inside one another sum to
                // at most the length of `text`.
                scan = header_start;
                continue;
            }

            let payload_start = header_end + 1;
            let end = Self::residual_payload_end(text, payload_start);

            let buffer = out.get_or_insert_with(|| String::with_capacity(text.len()));
            buffer.push_str(&text[copied..start]);
            buffer.push_str(TRUNCATED_DATA_NOTE);
            copied = end;
            scan = end;
        }

        match out {
            Some(mut buffer) => {
                buffer.push_str(&text[copied..]);
                Cow::Owned(buffer)
            }
            None => Cow::Borrowed(text),
        }
    }

    /// Byte length of the leading run of characters satisfying `allowed`.
    fn run_len(text: &str, allowed: fn(char) -> bool) -> usize {
        text.find(|ch: char| !allowed(ch)).unwrap_or(text.len())
    }

    /// End of a residual base64 run that starts at `payload_start`.
    ///
    /// A run normally ends at the first character outside the base64 alphabet.
    /// It continues into the next line only when the text is **uniformly
    /// line-wrapped**, which is what a wrapped payload looks like and what a list
    /// of long tokens does not. `crate::multimodal::parse_image_markers` only
    /// collapses a wrapped marker when it is terminated, so a payload that was
    /// line-wrapped and then truncated arrives with its newlines intact, and
    /// stopping at the first newline would leave every line but the first sitting
    /// in a text position — tens of thousands of prose tokens, which is the
    /// original bug.
    ///
    /// The continuation rule, in full. The gap between two segments must be
    /// exactly one line terminator — `\n`, `\r\n` or `\r` — so a space, an
    /// indent or a blank line ends the run. The width of the first continued
    /// line becomes the wrap width, and it is only accepted as a wrap width when
    /// either the payload's own first line is exactly that wide (a payload
    /// pre-wrapped by an encoder and then prefixed with a marker) or the whole
    /// line the header sits on is exactly that wide (text wrapped as a whole).
    /// Every later line must then match the same width, and the width itself must
    /// be at least [`WRAPPED_BASE64_WIDTH_MIN`].
    ///
    /// That is what keeps real tool output out of the run. A `sha256sum` listing
    /// after a quoted data URI has 64-character lines, but the header's line is
    /// not 64 characters wide and the payload's own first line is not either, so
    /// the run stops at the first newline and the digests survive. An earlier
    /// version continued across any whitespace into any segment of 64 or more
    /// base64 characters, which both ate such listings and missed every wrap
    /// width below 64.
    ///
    /// Two residues are accepted. The last line of a wrapped payload is shorter
    /// than the wrap width, so it stays in the text — at most a wrap width of
    /// base64 characters, sitting next to the note that says data was removed.
    /// Absorbing it would mean deleting whatever short word happens to follow a
    /// quoted data URI. And a payload wrapped with an indent on its continuation
    /// lines is not rejoined.
    fn residual_payload_end(text: &str, payload_start: usize) -> usize {
        let end = Self::residual_payload_run_end(text, payload_start);
        Self::without_trailing_scheme_prefix(text, payload_start, end)
    }

    /// Backs `end` off a trailing `data` when the byte at `end` is the `:` of
    /// another `data:`, so the overlapping occurrence is still examined.
    ///
    /// Every letter of `data` is in the base64 alphabet, so a payload run
    /// swallows the scheme name of a following data URI and stops at its colon.
    /// Resuming the scan there skipped the overlap entirely and left
    /// `:<media type>;base64,<payload>` in a text position. Ending the run before
    /// the scheme name instead moves the cursor *forward* from the run's start,
    /// so the sweep still advances and stays linear.
    fn without_trailing_scheme_prefix(text: &str, payload_start: usize, end: usize) -> usize {
        const SCHEME_NAME: &str = "data";

        if !text[end..].starts_with(':') {
            return end;
        }
        let Some(candidate) = end.checked_sub(SCHEME_NAME.len()) else {
            return end;
        };
        // Never reach back into the header: only payload bytes may be given up.
        if candidate < payload_start || &text[candidate..end] != SCHEME_NAME {
            return end;
        }
        candidate
    }

    /// End of the base64 run itself, before the overlap adjustment in
    /// [`Self::residual_payload_end`].
    fn residual_payload_run_end(text: &str, payload_start: usize) -> usize {
        let first_end =
            payload_start + Self::run_len(&text[payload_start..], is_base64_payload_char);
        let first_len = first_end - payload_start;
        let mut end = first_end;
        let mut wrap_width: Option<usize> = None;

        loop {
            let rest = &text[end..];
            let Some(gap) = Self::line_terminator_len(rest) else {
                // The run ended at punctuation, at a space, or at the end of the
                // string — not at a line break.
                return end;
            };
            let segment = Self::run_len(&rest[gap..], is_base64_payload_char);
            match wrap_width {
                Some(width) if segment == width => {}
                Some(_) => return end,
                None => {
                    // First continuation: decide whether this is wrapping at all.
                    if segment < WRAPPED_BASE64_WIDTH_MIN
                        || (segment != first_len
                            && !Self::line_ends_at_with_width(text, end, segment))
                    {
                        return end;
                    }
                    wrap_width = Some(segment);
                }
            }
            end += gap + segment;
        }
    }

    /// Length of the single line terminator at the start of `text`, or `None`
    /// when `text` does not start with exactly one.
    fn line_terminator_len(text: &str) -> Option<usize> {
        if let Some(rest) = text.strip_prefix('\r') {
            return Some(if rest.starts_with('\n') { 2 } else { 1 });
        }
        if text.starts_with('\n') {
            return Some(1);
        }
        None
    }

    /// True when the line ending at byte index `line_end` is exactly `width`
    /// bytes long.
    ///
    /// Byte length, not character count: a line holding a multi-byte character
    /// is simply not recognised as a wrapped line, which costs nothing but a
    /// sweep this function was never able to justify.
    fn line_ends_at_with_width(text: &str, line_end: usize, width: usize) -> bool {
        if line_end < width {
            return false;
        }
        let line_start = line_end - width;
        if line_start > 0 && !matches!(text.as_bytes()[line_start - 1], b'\n' | b'\r') {
            return false;
        }
        // `get` rejects a boundary that falls inside a multi-byte character.
        text.get(line_start..line_end)
            .is_some_and(|line| !line.contains(['\n', '\r']))
    }

    /// Top-level `image` and `text` blocks for a non-JSON tool message whose
    /// `tool_use_id` could not be recovered unambiguously.
    ///
    /// A `tool_result` block structurally requires a `tool_use_id`, and
    /// Anthropic rejects an id matching no `tool_use` in the preceding assistant
    /// turn. With no unambiguous id there is no correct `tool_result` to emit, so
    /// the choice is between top-level blocks that still deliver the image and
    /// inventing an id that draws a 400 or attaches the result to the wrong
    /// call. The image is delivered; only the correlation is lost, and it was
    /// already lost upstream.
    ///
    /// A text block is emitted **after** the images even when the prose is
    /// empty. `apply_cache_to_last_message` is a silent no-op on an `image`
    /// block, so an image in last position would cost the request its
    /// conversation cache breakpoint with nothing reporting it.
    fn unpaired_tool_output_blocks(content: &str) -> Vec<NativeContentOut> {
        let (cleaned, refs) = crate::multimodal::parse_image_markers(content);
        let (sources, omitted) = Self::deliverable_image_sources(&refs);

        let mut blocks: Vec<NativeContentOut> = Vec::with_capacity(sources.len() + 1);
        blocks.extend(
            sources
                .into_iter()
                .map(|source| NativeContentOut::Image { source }),
        );

        // With no references at all the sweep still has to run on the original
        // string, for the same reason as in `tool_result_content`.
        let base = if refs.is_empty() {
            content
        } else {
            cleaned.as_str()
        };
        let mut text = Self::text_with_omission_note(base, omitted);
        if text.is_empty() {
            text = IMAGE_ONLY_TEXT_PLACEHOLDER.to_string();
        }
        blocks.push(NativeContentOut::Text {
            text,
            cache_control: None,
        });
        blocks
    }

    /// Splits a native tool-result envelope into its `tool_use_id` and result
    /// text. `None` when the message is not such an envelope, which sends the
    /// caller down the non-JSON carrier path with the raw message.
    ///
    /// The presence of a `tool_call_id` key is what identifies the envelope.
    /// Requiring it means a tool that happens to return a JSON object with a
    /// `content` field keeps all of its fields, while an envelope whose id is
    /// unusable still gives up its payload instead of putting
    /// `{"tool_call_id":null,…}` in front of the model as if the tool had
    /// written it.
    ///
    /// A `content` value that is not a string is rendered as its JSON text
    /// rather than treated as absent, in **both** branches. A tool that returns
    /// a structured result meant that object to reach the model; dropping it
    /// left an empty `tool_result` on the wire with nothing saying so, and on the
    /// unusable-id branch it put the envelope scaffolding in front of the model
    /// instead. With no `content` key at all there is no payload, and the caller
    /// treats empty content as a message to skip.
    fn parse_tool_result_envelope(content: &str) -> Option<ToolResultEnvelope> {
        let value = serde_json::from_str::<serde_json::Value>(content).ok()?;
        let object = value.as_object()?;
        let id_field = object.get("tool_call_id")?;
        let result = object
            .get("content")
            .filter(|payload| !payload.is_null())
            .map(|payload| match payload.as_str() {
                Some(text) => text.to_string(),
                None => payload.to_string(),
            });
        Some(ToolResultEnvelope {
            tool_use_id: id_field.as_str().map(str::to_string),
            content: result.unwrap_or_default(),
        })
    }

    fn convert_messages(messages: &[ChatMessage]) -> (Option<SystemPrompt>, Vec<NativeMessage>) {
        let mut system_text = None;
        let mut native_messages = Vec::new();
        // The `tool_use` ids emitted by the most recent assistant message, and
        // the subset a tool result has already answered. Together they let the
        // non-JSON tool carrier below recover its `tool_use_id` when exactly one
        // candidate is left. Both are cleared by any message that ends the
        // tool-result run, so recovery only ever pairs a result with the
        // assistant turn it actually follows.
        let mut pending_tool_use_ids: Vec<String> = Vec::new();
        let mut answered_tool_use_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for (index, msg) in messages.iter().enumerate() {
            if ChatMessage::should_skip_internal_pruning_marker(messages, index) {
                continue;
            }
            match msg.role.as_str() {
                "system" => {
                    // A system message is not emitted into the message list — it
                    // becomes the request's `system` field, or is dropped — so it
                    // cannot break the adjacency between a `tool_use` and its
                    // `tool_result` on the wire, which is the adjacency the
                    // candidate set exists to protect. It therefore does not end
                    // a tool-result run. The same holds for the other messages
                    // that produce no wire content: an empty assistant message
                    // and a skipped pruning marker.
                    if system_text.is_none() {
                        system_text = Some(msg.content.clone());
                    }
                }
                "assistant" => {
                    if let Some(blocks) = Self::parse_assistant_tool_call_message(&msg.content) {
                        pending_tool_use_ids = blocks
                            .iter()
                            .filter_map(|block| match block {
                                NativeContentOut::ToolUse { id, .. } => Some(id.clone()),
                                _ => None,
                            })
                            .collect();
                        answered_tool_use_ids.clear();
                        native_messages.push(NativeMessage {
                            role: "assistant".to_string(),
                            content: blocks,
                        });
                    } else if !msg.content.trim().is_empty() {
                        // An assistant message without tool calls ends the run.
                        pending_tool_use_ids.clear();
                        answered_tool_use_ids.clear();
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
                    let envelope = Self::parse_tool_result_envelope(&msg.content);
                    // The tool's own output: the envelope's `content` when this
                    // is an envelope at all, the raw message otherwise.
                    let carrier = match &envelope {
                        Some(parsed) => parsed.content.as_str(),
                        None => msg.content.as_str(),
                    };
                    let tool_msg = if let Some(tool_use_id) = envelope
                        .as_ref()
                        .and_then(|parsed| parsed.tool_use_id.clone())
                    {
                        answered_tool_use_ids.insert(tool_use_id.clone());
                        NativeMessage {
                            role: "user".to_string(),
                            content: vec![NativeContentOut::ToolResult {
                                tool_use_id,
                                content: Self::tool_result_content(carrier),
                                cache_control: None,
                            }],
                        }
                    } else if !carrier.trim().is_empty() {
                        // Non-JSON tool carrier: `ChatMessage::tool` accepts any
                        // string, and an envelope with a non-string
                        // `tool_call_id` lands here too. Recover the id when the
                        // assistant turn this message still follows left exactly
                        // one call unanswered — that also stops
                        // `backfill_orphaned_tool_uses` from putting a "tool
                        // result missing" stub next to the real result.
                        let recovered = {
                            let mut unanswered = pending_tool_use_ids
                                .iter()
                                .filter(|id| !answered_tool_use_ids.contains(*id));
                            match (unanswered.next(), unanswered.next()) {
                                (Some(only), None) => Some(only.clone()),
                                // Zero candidates, or two or more: the pairing is
                                // ambiguous and an id is never invented.
                                _ => None,
                            }
                        };
                        match recovered {
                            Some(tool_use_id) => {
                                answered_tool_use_ids.insert(tool_use_id.clone());
                                NativeMessage {
                                    role: "user".to_string(),
                                    content: vec![NativeContentOut::ToolResult {
                                        tool_use_id,
                                        content: Self::tool_result_content(carrier),
                                        cache_control: None,
                                    }],
                                }
                            }
                            None => NativeMessage {
                                role: "user".to_string(),
                                content: Self::unpaired_tool_output_blocks(carrier),
                            },
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
                    // A user message ends the tool-result run, so a later
                    // non-JSON tool message must not be paired with the assistant
                    // turn before it.
                    pending_tool_use_ids.clear();
                    answered_tool_use_ids.clear();

                    // Parse image markers from user message content
                    let (text, image_refs) = crate::multimodal::parse_image_markers(&msg.content);
                    let mut content_blocks: Vec<NativeContentOut> = Vec::new();
                    let mut omitted = 0usize;

                    // Add image content blocks for each image reference
                    for img_ref in &image_refs {
                        let (media_type, data) = if img_ref.starts_with("data:") {
                            // Routed through the same shared structural check the
                            // tool arms use, so every arm agrees on what a
                            // deliverable image is. Stricter than the old
                            // split-on-first-comma: a header without `;base64`, a
                            // media type off the allowlist, a non-canonical
                            // payload, or one over the per-image ceiling is now
                            // skipped here instead of drawing a 400 from the API.
                            match crate::multimodal::split_base64_image_data_uri(
                                img_ref,
                                MAX_ENCODED_IMAGE_PAYLOAD_BYTES,
                            ) {
                                Ok((mime, payload)) => {
                                    (mime.to_ascii_lowercase(), payload.to_string())
                                }
                                Err(_) => {
                                    omitted += 1;
                                    continue;
                                }
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
                                Err(_) => {
                                    omitted += 1;
                                    continue;
                                }
                            }
                        } else {
                            omitted += 1;
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

                    // Every reference that produced no block is counted, so a
                    // message whose images were all rejected says so instead of
                    // serializing as empty content.
                    let text = Self::text_with_omission_note(&text, omitted);

                    // The `[image]` placeholder is gated on a block having been
                    // built, not on references existing: after the stricter
                    // validation above a reference can be present and produce
                    // nothing, and telling the model an image is attached with
                    // none on the wire is worse than saying nothing.
                    if text.is_empty() && !content_blocks.is_empty() {
                        content_blocks.push(NativeContentOut::Text {
                            text: IMAGE_ONLY_TEXT_PLACEHOLDER.to_string(),
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

        Self::dedupe_tool_results_by_id(&mut native_messages);
        Self::order_tool_results_first(&mut native_messages);
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

    /// Keep at most one `tool_result` per `tool_use_id` in each user-role
    /// message, demoting any later duplicate to top-level blocks.
    ///
    /// Anthropic accepts one `tool_result` per `tool_use`; answering the same
    /// call twice in one message is a 400. That shape is reachable: the non-JSON
    /// carrier recovers the single outstanding id from the assistant turn it
    /// follows, and a JSON envelope naming that same id later in the same run
    /// merges into the same user message. A history restored with the same
    /// envelope twice reaches it too.
    ///
    /// The first block wins, because it is the one adjacent to its `tool_use`.
    /// The later one is not thrown away: its blocks move to the end of the
    /// message as top-level content, so its output — including any image — still
    /// reaches the model, with only the correlation lost. Runs before
    /// [`Self::order_tool_results_first`], which then moves the surviving
    /// `tool_result` blocks back to the front.
    fn dedupe_tool_results_by_id(messages: &mut [NativeMessage]) {
        for message in messages.iter_mut() {
            if message.role != "user" {
                continue;
            }
            // A message with fewer than two `tool_result` blocks cannot hold a
            // duplicate, which is every message this converter normally builds.
            // Checking the count first keeps the common path off the allocating
            // path below: `convert_messages` runs over the whole replayed history
            // on every turn.
            let mut ids: Vec<&str> = Vec::new();
            for block in &message.content {
                if let NativeContentOut::ToolResult { tool_use_id, .. } = block {
                    ids.push(tool_use_id.as_str());
                }
            }
            if ids.len() < 2 {
                continue;
            }
            // A handful of blocks per message, so a linear scan beats hashing and
            // allocates nothing.
            let duplicated = ids
                .iter()
                .enumerate()
                .any(|(index, id)| ids[..index].contains(id));
            if !duplicated {
                continue;
            }

            let mut seen: Vec<String> = Vec::new();
            let mut kept: Vec<NativeContentOut> = Vec::with_capacity(message.content.len());
            let mut demoted: Vec<NativeContentOut> = Vec::new();
            for block in std::mem::take(&mut message.content) {
                match block {
                    NativeContentOut::ToolResult {
                        tool_use_id,
                        content,
                        cache_control,
                    } => {
                        if seen.contains(&tool_use_id) {
                            demoted.extend(Self::demoted_tool_result_blocks(&tool_use_id, content));
                        } else {
                            seen.push(tool_use_id.clone());
                            kept.push(NativeContentOut::ToolResult {
                                tool_use_id,
                                content,
                                cache_control,
                            });
                        }
                    }
                    other => kept.push(other),
                }
            }
            kept.extend(demoted);
            message.content = kept;
        }
    }

    /// Top-level blocks for a `tool_result` dropped because an earlier block in
    /// the same message already answered its `tool_use`.
    ///
    /// Images come first and text last, for the same reason as
    /// [`Self::unpaired_tool_output_blocks`]: `apply_cache_to_last_message` is a
    /// silent no-op on an `image` block, so a message ending on one loses its
    /// conversation cache breakpoint with nothing reporting it. An empty tool
    /// result yields no blocks at all rather than a placeholder claiming an
    /// image is attached.
    ///
    /// The text block names what it is, with the `tool_use_id` it came from. A
    /// top-level block in a user message otherwise reads as something the user
    /// typed, so the model would take a tool's second answer — or a bare
    /// `[image]` — as a user attachment.
    fn demoted_tool_result_blocks(
        tool_use_id: &str,
        content: ToolResultContent,
    ) -> Vec<NativeContentOut> {
        let mut blocks: Vec<NativeContentOut> = Vec::new();
        let mut texts: Vec<String> = Vec::new();
        match content {
            ToolResultContent::Text(text) => texts.push(text),
            ToolResultContent::Blocks(nested) => {
                for block in nested {
                    match block {
                        ToolResultBlock::Text { text } => texts.push(text),
                        ToolResultBlock::Image { source } => {
                            blocks.push(NativeContentOut::Image { source });
                        }
                    }
                }
            }
        }

        let body = texts.join("\n");
        let body = body.trim();
        if body.is_empty() && blocks.is_empty() {
            return blocks;
        }
        let label = format!("{DEMOTED_TOOL_RESULT_PREFIX} {tool_use_id}]");
        let text = if body.is_empty() {
            label
        } else {
            format!("{label}\n{body}")
        };
        blocks.push(NativeContentOut::Text {
            text,
            cache_control: None,
        });
        blocks
    }

    /// Move `tool_result` blocks to the front of every user-role message,
    /// preserving relative order within each group.
    ///
    /// Anthropic returns a 400 when text precedes a `tool_result` in the same
    /// user message. This converter merges consecutive tool messages into one
    /// user message and merges a user message into a preceding user-role
    /// message — and a converted tool result *is* a user-role message — so any
    /// user-role text immediately before a tool result lands in the same message
    /// with the text first.
    ///
    /// Runs before [`Self::backfill_orphaned_tool_uses`] so the stub inserter
    /// sees final ordering; the backfill prepends its stubs, so the invariant
    /// still holds afterwards. Assistant messages are untouched.
    fn order_tool_results_first(messages: &mut [NativeMessage]) {
        for message in messages.iter_mut() {
            if message.role != "user" {
                continue;
            }
            let out_of_order = message
                .content
                .iter()
                .skip_while(|block| matches!(block, NativeContentOut::ToolResult { .. }))
                .any(|block| matches!(block, NativeContentOut::ToolResult { .. }));
            if !out_of_order {
                continue;
            }
            let (tool_results, others): (Vec<NativeContentOut>, Vec<NativeContentOut>) =
                std::mem::take(&mut message.content)
                    .into_iter()
                    .partition(|block| matches!(block, NativeContentOut::ToolResult { .. }));
            message.content = tool_results.into_iter().chain(others).collect();
        }
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
                    content: ToolResultContent::Text(
                        "[tool result missing from history — the turn was \
                         interrupted before this tool finished]"
                            .to_string(),
                    ),
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
            self.timeout_secs,
            10,
        )
    }

    /// Streaming requests have no whole-request deadline. Header acquisition
    /// and buffered error bodies are bounded separately, while successful SSE
    /// bodies use the shared byte-idle timeout.
    fn streaming_http_client(&self) -> Result<Client, reqwest::Error> {
        let builder = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .read_timeout(STREAM_IDLE_TIMEOUT);
        let builder = zeroclaw_config::schema::apply_runtime_proxy_to_builder(
            builder,
            "model_provider.anthropic",
        );
        builder.build()
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

        let mut input_tokens: Option<u64> = None;
        let mut output_tokens: Option<u64> = None;
        let mut cached_input_tokens: Option<u64> = None;
        let mut cache_creation_input_tokens: Option<u64> = None;

        loop {
            let line = match lines.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(err) => {
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

        crate::stream_guard::finish_sse_stream(tx, false, "message_stop").await;
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
                Some(ToolSpec::new(
                    name.to_string(),
                    func.get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string(),
                    func.get("parameters")
                        .cloned()
                        .unwrap_or(serde_json::json!({"type": "object"})),
                ))
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
        let client = match self.streaming_http_client() {
            Ok(client) => client,
            Err(error) => {
                let message = format!(
                    "Failed to build Anthropic streaming client: {}",
                    super::format_error_chain(&error)
                );
                return stream::once(async move { Err(StreamError::Http(message)) }).boxed();
            }
        };
        let url = format!("{}/v1/messages", self.base_url);
        let is_oauth = Self::is_setup_token(&credential);
        let phase_timeout = std::time::Duration::from_secs(self.timeout_secs);

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(64);

        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Spawn)
                .with_category(::zeroclaw_log::EventCategory::Provider)
                .with_attrs(::serde_json::json!({
                    "idle_timeout_secs": STREAM_IDLE_TIMEOUT.as_secs(),
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

            let response = match tokio::time::timeout(phase_timeout, req.send()).await {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    let _ = tx
                        .send(Err(StreamError::Http(super::format_error_chain(&e))))
                        .await;
                    return;
                }
                Err(_) => {
                    let _ = tx
                        .send(Err(StreamError::Http(format!(
                            "streaming response headers not received within {}s",
                            phase_timeout.as_secs()
                        ))))
                        .await;
                    return;
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let error = match tokio::time::timeout(phase_timeout, response.text()).await {
                    Ok(Ok(body)) => body,
                    Ok(Err(error)) => format!("error response body read failed: {error}"),
                    Err(_) => format!(
                        "error response body not received within {}s",
                        phase_timeout.as_secs()
                    ),
                };
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
        // of a leaked socket that lingers until STREAM_IDLE_TIMEOUT.
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

    /// Canonical base64 for a 1x1 PNG: 68 characters, a multiple of four,
    /// standard alphabet, no padding. Anything shorter that merely looks like a
    /// PNG prefix is not canonical base64 and is rejected before it reaches the
    /// wire.
    const CANONICAL_PNG_B64: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAAAAAA6fptVAAAACklEQVR4nGMAAQAABQAB";
    /// A second canonical payload, so a test with two images can tell them
    /// apart. Decodes to a JPEG SOI + APP0 header.
    const CANONICAL_JPEG_B64: &str = "/9j/4AAQ";
    /// The omission note for a single rejected reference, spelled out once so
    /// the tests pin the exact prompt text the model reads.
    const OMISSION_NOTE_ONE: &str =
        "[1 image(s) omitted: unsupported or oversized image reference]";

    /// Serializes converted messages and returns the first `tool_result` block
    /// as JSON. Assertions go through the wire shape because that is where the
    /// difference between a bare string and a block list lives.
    fn first_tool_result_on_the_wire(native_msgs: &[NativeMessage]) -> serde_json::Value {
        let wire = serde_json::to_value(native_msgs).expect("serialize native messages");
        wire.as_array()
            .expect("messages array")
            .iter()
            .flat_map(|message| message["content"].as_array().expect("content array").iter())
            .find(|block| block["type"] == "tool_result")
            .cloned()
            .expect("a tool_result block")
    }

    /// Every `tool_result` block on the wire, in message then block order.
    fn tool_results_on_the_wire(native_msgs: &[NativeMessage]) -> Vec<serde_json::Value> {
        let wire = serde_json::to_value(native_msgs).expect("serialize native messages");
        wire.as_array()
            .expect("messages array")
            .iter()
            .flat_map(|message| message["content"].as_array().expect("content array").iter())
            .filter(|block| block["type"] == "tool_result")
            .cloned()
            .collect()
    }

    /// The content blocks of the last user-role message on the wire. That is the
    /// message tool results merge into, and the one
    /// `apply_cache_to_last_message` writes its breakpoint to.
    fn last_user_blocks(native_msgs: &[NativeMessage]) -> Vec<serde_json::Value> {
        let wire = serde_json::to_value(native_msgs).expect("serialize native messages");
        wire.as_array()
            .expect("messages array")
            .iter()
            .rfind(|message| message["role"] == "user")
            .expect("a user message")["content"]
            .as_array()
            .expect("content array")
            .clone()
    }

    /// Index of the first block of `kind` in a block list.
    fn block_position(blocks: &[serde_json::Value], kind: &str) -> Option<usize> {
        blocks.iter().position(|block| block["type"] == kind)
    }

    /// Every string held by a `text` field anywhere in a JSON tree.
    fn text_fields(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    if key == "text"
                        && let Some(text) = child.as_str()
                    {
                        out.push(text.to_string());
                    }
                    text_fields(child, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    text_fields(item, out);
                }
            }
            _ => {}
        }
    }

    /// A history whose last message is a JSON tool-result envelope answering a
    /// single `tool_use`, so the converted `tool_result` is well-formed and the
    /// cache breakpoint lands on it.
    fn history_with_tool_result(result_text: &str) -> Vec<ChatMessage> {
        vec![
            ChatMessage::system("You take screenshots."),
            ChatMessage::user("take a screenshot"),
            ChatMessage::assistant(
                serde_json::json!({
                    "content": "",
                    "tool_calls": [
                        {"id": "toolu_screenshot", "name": "screenshot", "arguments": "{}"}
                    ]
                })
                .to_string(),
            ),
            ChatMessage::tool(
                serde_json::json!({
                    "tool_call_id": "toolu_screenshot",
                    "content": result_text,
                })
                .to_string(),
            ),
        ]
    }

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
        // The originallive repro was Anthropic streaming; before this
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
    async fn dropping_guard_aborts_parser_without_idle_wait() {
        // The full-measure fix: dropping the consumer stream must abort the
        // detached parser immediately (turn cancel), not leak the socket until
        // STREAM_IDLE_TIMEOUT. We model the stream's lifetime with AbortOnDrop and
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

        // Dropping the guard must abort the parser — no STREAM_IDLE_TIMEOUT wait.
        drop(guard);
        tokio::task::yield_now().await;
        assert!(
            probe.is_finished(),
            "guard drop must abort the parser task immediately, not wait out the idle timeout"
        );
    }

    #[tokio::test]
    async fn successful_stream_can_outlive_configured_request_timeout() {
        use axum::{Router, response::IntoResponse, routing::post};
        use futures_util::StreamExt as _;

        let app = Router::new().route(
            "/v1/messages",
            post(|| async {
                let first = futures_util::stream::once(async {
                    Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(
                        b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
                    ))
                });
                let terminal = futures_util::stream::once(async {
                    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                    Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(
                        b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\ndata: {\"type\":\"message_stop\"}\n\n",
                    ))
                });
                axum::body::Body::from_stream(first.chain(terminal)).into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind Anthropic SSE test server");
        let addr = listener.local_addr().expect("Anthropic SSE test address");
        let server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app)
                .await
                .expect("serve Anthropic SSE test");
        });
        let provider = AnthropicModelProvider::builder("test")
            .credential(Some("test-key"))
            .base_url(&format!("http://{addr}"))
            .timeout_secs(1)
            .build();
        let messages = vec![ChatMessage::user("hi")];
        let mut stream = provider.stream_chat(
            ProviderChatRequest {
                messages: &messages,
                tools: None,
                thinking: None,
            },
            "claude-haiku-4-5",
            None,
            StreamOptions {
                enabled: true,
                count_tokens: false,
            },
        );
        let mut text = String::new();
        let mut saw_final = false;

        tokio::time::timeout(std::time::Duration::from_secs(4), async {
            while let Some(event) = stream.next().await {
                match event.expect("successful SSE stream must not fail") {
                    StreamEvent::TextDelta(chunk) => text.push_str(&chunk.delta),
                    StreamEvent::Final => {
                        saw_final = true;
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("successful stream must finish after exceeding the request timeout");

        server.abort();
        assert_eq!(text, "hi");
        assert!(saw_final, "message_stop must emit Final");
    }

    #[tokio::test]
    async fn eof_before_message_stop_surfaces_error_not_final() {
        use std::io::Cursor;

        let bytes = b"event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude\",\"usage\":{\"input_tokens\":10}}}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n";
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
        let p = AnthropicModelProvider::builder("test")
            .credential(Some("anthropic-test-credential"))
            .build();
        assert!(p.credential.is_some());
        assert_eq!(p.credential.as_deref(), Some("anthropic-test-credential"));
        assert_eq!(p.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn creates_without_key() {
        let p = AnthropicModelProvider::builder("test").build();
        assert!(p.credential.is_none());
        assert_eq!(p.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn creates_with_empty_key() {
        let p = AnthropicModelProvider::builder("test")
            .credential(Some(""))
            .build();
        assert!(p.credential.is_none());
    }

    #[test]
    fn creates_with_whitespace_key() {
        let p = AnthropicModelProvider::builder("test")
            .credential(Some("  anthropic-test-credential  "))
            .build();
        assert!(p.credential.is_some());
        assert_eq!(p.credential.as_deref(), Some("anthropic-test-credential"));
    }

    #[test]
    fn creates_with_custom_base_url() {
        let p = AnthropicModelProvider::builder("test")
            .credential(Some("anthropic-credential"))
            .base_url("https://api.example.com")
            .build();
        assert_eq!(p.base_url, "https://api.example.com");
        assert_eq!(p.credential.as_deref(), Some("anthropic-credential"));
    }

    #[test]
    fn custom_base_url_trims_trailing_slash() {
        let p = AnthropicModelProvider::builder("test")
            .base_url("https://api.example.com/")
            .build();
        assert_eq!(p.base_url, "https://api.example.com");
    }

    #[test]
    fn no_base_url_uses_published_endpoint() {
        let p = AnthropicModelProvider::builder("test").build();
        assert_eq!(p.base_url, "https://api.anthropic.com");
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        let p = AnthropicModelProvider::builder("test").build();
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
        let model_provider = AnthropicModelProvider::builder("test").build();
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
        let model_provider = AnthropicModelProvider::builder("test").build();
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
        let p = AnthropicModelProvider::builder("test").build();
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
        let provider = AnthropicModelProvider::builder("test")
            .credential(Some("test-key"))
            .build();
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
        let provider = AnthropicModelProvider::builder("test")
            .credential(Some("test-key"))
            .build();
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
            content: ToolResultContent::Text("Result data".to_string()),
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
                content: ToolResultContent::Text("Result".to_string()),
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
            ToolSpec::new("tool1", "First tool", serde_json::json!({"type": "object"})),
            ToolSpec::new(
                "tool2",
                "Second tool",
                serde_json::json!({"type": "object"}),
            ),
        ];

        let native_tools = AnthropicModelProvider::convert_tools(Some(&tools)).unwrap();

        assert_eq!(native_tools.len(), 2);
        assert!(native_tools[0].cache_control.is_none());
        assert!(native_tools[1].cache_control.is_some());
    }

    #[test]
    fn convert_tools_single_tool_gets_cache() {
        let tools = vec![ToolSpec::new(
            "tool1",
            "Only tool",
            serde_json::json!({"type": "object"}),
        )];

        let native_tools = AnthropicModelProvider::convert_tools(Some(&tools)).unwrap();

        assert_eq!(native_tools.len(), 1);
        assert!(native_tools[0].cache_control.is_some());
    }

    #[test]
    fn convert_tools_cleans_ref_from_input_schema() {
        let tools = vec![ToolSpec::new(
            "query",
            "Search with a ref",
            serde_json::json!({
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
            }),
        )];

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
        let tools = vec![ToolSpec::new(
            "query",
            "Search with a definitions ref",
            serde_json::json!({
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
            }),
        )];

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
        let model_provider = AnthropicModelProvider::builder("test").build();
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
            timeout_secs: 120,
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
        let model_provider = AnthropicModelProvider::builder("test")
            .credential(Some("test-key"))
            .build();
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
        // Payload is the canonical 1x1 PNG. The 11-character `iVBORw0KGgo`
        // this fixture used before is not a multiple of four, so it is not
        // canonical base64 and Anthropic's decoder would reject it.
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: format!("[IMAGE:data:image/png;base64,{CANONICAL_PNG_B64}]"),
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

    #[tokio::test]
    async fn anthropic_factory_forwards_timeout_to_native_provider() {
        use crate::ModelProviderRuntimeOptions;
        use crate::factory::FamilyProviderFactory;
        use axum::{Json, Router, routing::post};
        use serde_json::json;
        use tokio::time::{Duration, Instant};
        use zeroclaw_config::schema::AnthropicModelProviderConfig;

        async fn slow_messages() -> Json<serde_json::Value> {
            tokio::time::sleep(Duration::from_secs(3)).await;
            Json(json!({
                "id": "msg_late",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "text", "text": "too late"}],
                "model": "claude-sonnet-4-5",
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 1, "output_tokens": 1}
            }))
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("test server addr");
        let app = Router::new().route("/v1/messages", post(slow_messages));
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.expect("serve test server");
        });

        let opts = ModelProviderRuntimeOptions {
            provider_timeout_secs: Some(1),
            ..Default::default()
        };
        let provider = AnthropicModelProviderConfig::default()
            .create_provider(
                "native",
                Some("test-key"),
                Some(&format!("http://{addr}")),
                &opts,
            )
            .expect("anthropic provider should build");

        let started = Instant::now();
        let result = provider
            .chat_with_system(None, "hello", "claude-sonnet-4-5", Some(0.7))
            .await;
        let elapsed = started.elapsed();

        server.abort();

        assert!(
            result.is_err(),
            "slow response should time out when factory forwards provider_timeout_secs"
        );
        assert!(
            elapsed < Duration::from_secs(3),
            "request waited for the server response instead of using configured timeout: {elapsed:?}"
        );
    }

    /// The issue's exact repro, through the mock server so the assertion is on
    /// the body actually posted: a normalized image marker inside a native tool
    /// result must reach Anthropic as an `image` block nested in the
    /// `tool_result`, with the base64 only ever inside a `source`.
    ///
    /// Before the change this fails on the nested block: `tool_result.content`
    /// was a `String`, so no image block could exist inside a tool result
    /// anywhere. The "no base64 in a text position" half already passed on
    /// `774fc36cd` — the payload was simply dropped — so the nested-block
    /// assertion is the only discriminator here.
    #[tokio::test]
    async fn tool_result_image_delivered_as_nested_block() {
        use axum::{Json, Router, routing::post};
        use std::sync::{Arc, Mutex};
        use tokio::net::TcpListener;

        let captured: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let captured_clone = captured.clone();

        let app = Router::new().route(
            "/v1/messages",
            post(move |Json(body): Json<serde_json::Value>| {
                let cap = captured_clone.clone();
                async move {
                    *cap.lock().expect("capture lock") = Some(body);
                    Json(serde_json::json!({
                        "id": "msg_test",
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "text", "text": "I see a 1x1 pixel."}],
                        "model": "claude-opus-4-6",
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 100, "output_tokens": 20}
                    }))
                }
            }),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let server_handle = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.expect("serve");
        });

        let model_provider = AnthropicModelProvider {
            alias: "test".to_string(),
            credential: Some("test-key".to_string()),
            base_url: format!("http://{addr}"),
            max_tokens: 4096,
            timeout_secs: 120,
        };

        let messages = history_with_tool_result(&format!(
            "saved screenshot [IMAGE:data:image/png;base64,{CANONICAL_PNG_B64}]"
        ));
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "screenshot",
                "description": "Take a screenshot",
                "parameters": {"type": "object", "properties": {}}
            }
        })];

        let result = model_provider
            .chat_with_tools(&messages, &tools, "claude-opus-4-6", Some(0.7))
            .await;
        assert!(result.is_ok(), "chat_with_tools failed: {:?}", result.err());

        let body = captured
            .lock()
            .expect("capture lock")
            .take()
            .expect("no request captured");

        let tool_result = body["messages"]
            .as_array()
            .expect("messages array")
            .iter()
            .flat_map(|message| message["content"].as_array().expect("content array").iter())
            .find(|block| block["type"] == "tool_result")
            .cloned()
            .expect("a tool_result block in the posted body");

        assert_eq!(tool_result["tool_use_id"], "toolu_screenshot");

        let blocks = tool_result["content"]
            .as_array()
            .unwrap_or_else(|| panic!("tool_result.content must be a block list: {tool_result}"));
        let image = blocks
            .iter()
            .find(|block| block["type"] == "image")
            .unwrap_or_else(|| panic!("no image block nested in the tool_result: {tool_result}"));
        assert_eq!(image["source"]["type"], "base64");
        assert_eq!(image["source"]["media_type"], "image/png");
        assert_eq!(image["source"]["data"], CANONICAL_PNG_B64);

        assert!(
            blocks.iter().any(|block| block["type"] == "text"
                && block["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("saved screenshot"))),
            "the prose around the image must survive: {tool_result}"
        );

        // The payload occurs exactly once in the whole posted body, and that
        // occurrence is the `source.data` asserted above.
        let posted = body.to_string();
        assert_eq!(
            posted.matches(CANONICAL_PNG_B64).count(),
            1,
            "base64 payload must appear exactly once, inside `source`: {posted}"
        );
        let mut texts = Vec::new();
        text_fields(&body, &mut texts);
        assert!(
            texts.iter().all(|text| !text.contains(CANONICAL_PNG_B64)),
            "base64 payload must never sit in a text position: {texts:?}"
        );

        // The rest of the request assembly still holds with block content:
        // system prompt, tool specs, and the conversation cache breakpoint.
        assert!(
            body["system"].to_string().contains("You take screenshots."),
            "system prompt missing: {}",
            body["system"]
        );
        assert_eq!(
            body["tools"]
                .as_array()
                .expect("tools array")
                .first()
                .expect("one tool")["name"],
            "screenshot"
        );
        assert_eq!(
            tool_result["cache_control"]["type"], "ephemeral",
            "the posted request lost its cache breakpoint: {tool_result}"
        );

        server_handle.abort();
    }

    /// Two valid data URIs plus prose: both images are delivered, in reference
    /// order, after the text block.
    ///
    /// Before the change both payloads were stripped and replaced by an
    /// omission note, so there were zero image blocks.
    #[test]
    fn tool_result_with_several_images() {
        let messages = history_with_tool_result(&format!(
            "two shots [IMAGE:data:image/png;base64,{CANONICAL_PNG_B64}] \
             and [IMAGE:data:image/jpeg;base64,{CANONICAL_JPEG_B64}]"
        ));

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);
        let tool_result = first_tool_result_on_the_wire(&native_msgs);
        let blocks = tool_result["content"]
            .as_array()
            .unwrap_or_else(|| panic!("expected a block list: {tool_result}"));

        assert_eq!(blocks.len(), 3, "expected text + two images: {tool_result}");
        assert_eq!(blocks[0]["type"], "text");
        assert!(
            blocks[0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("two shots")),
            "prose must lead the block list: {tool_result}"
        );
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["media_type"], "image/png");
        assert_eq!(blocks[1]["source"]["data"], CANONICAL_PNG_B64);
        assert_eq!(blocks[2]["type"], "image");
        assert_eq!(blocks[2]["source"]["media_type"], "image/jpeg");
        assert_eq!(blocks[2]["source"]["data"], CANONICAL_JPEG_B64);

        let wire = serde_json::to_string(&native_msgs).expect("serialize");
        assert_eq!(wire.matches(CANONICAL_PNG_B64).count(), 1);
        assert_eq!(wire.matches(CANONICAL_JPEG_B64).count(), 1);
        assert!(
            !wire.contains("image(s) omitted"),
            "nothing was omitted, so no note belongs here: {wire}"
        );
    }

    /// One deliverable PNG, one media type outside the allowlist, one
    /// `http://` URL: one image block plus a note counting the other two.
    ///
    /// Before the change this fails twice over: there were zero image blocks,
    /// and the note counted three, because `parse_image_markers` treats an
    /// `http://` reference as loadable and the old code counted every
    /// reference it saw.
    #[test]
    fn tool_result_mixes_valid_and_rejected_images() {
        let svg_payload = "PHN2Zz48L3N2Zz4=";
        let messages = history_with_tool_result(&format!(
            "mixed bag [IMAGE:data:image/png;base64,{CANONICAL_PNG_B64}] \
             [IMAGE:data:image/svg+xml;base64,{svg_payload}] \
             [IMAGE:http://example.com/remote.png]"
        ));

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);
        let tool_result = first_tool_result_on_the_wire(&native_msgs);
        let blocks = tool_result["content"]
            .as_array()
            .unwrap_or_else(|| panic!("expected a block list: {tool_result}"));

        let images: Vec<&serde_json::Value> = blocks
            .iter()
            .filter(|block| block["type"] == "image")
            .collect();
        assert_eq!(
            images.len(),
            1,
            "only the PNG is deliverable: {tool_result}"
        );
        assert_eq!(images[0]["source"]["data"], CANONICAL_PNG_B64);

        let text = blocks
            .iter()
            .find(|block| block["type"] == "text")
            .and_then(|block| block["text"].as_str())
            .unwrap_or_else(|| panic!("expected a text block: {tool_result}"));
        assert!(text.contains("mixed bag"), "prose must survive: {text}");
        assert!(
            text.contains("[2 image(s) omitted: unsupported or oversized image reference]"),
            "the two rejected references must be counted, and only those: {text}"
        );

        let wire = serde_json::to_string(&native_msgs).expect("serialize");
        assert!(
            !wire.contains(svg_payload),
            "a rejected payload must not reach the wire: {wire}"
        );
        assert!(
            !wire.contains("example.com"),
            "a rejected remote reference must not reach the wire: {wire}"
        );
    }

    /// An image with no prose around it produces an image block and no empty
    /// text block.
    ///
    /// Before the change the content became the bare omission note.
    #[test]
    fn tool_result_image_only_has_no_empty_text_block() {
        let messages = history_with_tool_result(&format!(
            "[IMAGE:data:image/png;base64,{CANONICAL_PNG_B64}]"
        ));

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);
        let tool_result = first_tool_result_on_the_wire(&native_msgs);
        let blocks = tool_result["content"]
            .as_array()
            .unwrap_or_else(|| panic!("expected a block list: {tool_result}"));

        assert_eq!(blocks.len(), 1, "expected the image alone: {tool_result}");
        assert_eq!(blocks[0]["type"], "image");
        assert_eq!(blocks[0]["source"]["data"], CANONICAL_PNG_B64);
    }

    /// Each rejection class, every case carrying a deliverable PNG alongside
    /// the rejected reference.
    ///
    /// The valid sibling is what makes this fail before the change: without it
    /// the old converter's "no image block plus an omission note" is already
    /// true for all-rejected input, because it stripped every reference
    /// regardless of validity. The exact note wording is asserted too — the old
    /// wording claimed Anthropic tool results cannot carry images, which is
    /// false.
    #[test]
    fn rejected_tool_result_data_uris_are_counted_not_sent() {
        // Over the 10 MB encoded ceiling. Canonical base64 otherwise, so the
        // size is the only thing wrong with it.
        let oversized = "A".repeat(MAX_ENCODED_IMAGE_PAYLOAD_BYTES + 4);
        let cases: Vec<(&str, String)> = vec![
            (
                "header does not declare ;base64",
                format!("data:image/png,{CANONICAL_JPEG_B64}"),
            ),
            (
                "media type outside the allowlist",
                "data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=".to_string(),
            ),
            (
                "payload length is not a multiple of four",
                "data:image/gif;base64,R0lGODlhAQABAA".to_string(),
            ),
            (
                "payload over the encoded ceiling",
                format!("data:image/png;base64,{oversized}"),
            ),
        ];

        for (label, rejected) in cases {
            let messages = history_with_tool_result(&format!(
                "prose [IMAGE:{rejected}] [IMAGE:data:image/png;base64,{CANONICAL_PNG_B64}]"
            ));

            let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);
            let tool_result = first_tool_result_on_the_wire(&native_msgs);
            let blocks = tool_result["content"]
                .as_array()
                .unwrap_or_else(|| panic!("{label}: expected a block list"));

            let images: Vec<&serde_json::Value> = blocks
                .iter()
                .filter(|block| block["type"] == "image")
                .collect();
            assert_eq!(
                images.len(),
                1,
                "{label}: only the valid sibling should become an image block"
            );
            assert_eq!(images[0]["source"]["data"], CANONICAL_PNG_B64, "{label}");

            let text = blocks
                .iter()
                .find(|block| block["type"] == "text")
                .and_then(|block| block["text"].as_str())
                .unwrap_or_else(|| panic!("{label}: expected a text block"));
            assert!(
                text.contains(OMISSION_NOTE_ONE),
                "{label}: expected the omission note, got {text}"
            );

            let wire = serde_json::to_string(&native_msgs).expect("serialize");
            let rejected_payload = rejected
                .rsplit(',')
                .next()
                .expect("data URI payload after the comma");
            assert!(
                !wire.contains(rejected_payload),
                "{label}: the rejected payload must not reach the wire"
            );
        }
    }

    /// A tool result whose content is a block list still takes the conversation
    /// cache breakpoint.
    ///
    /// The cache-control half alone already passes: `cache_control` is a
    /// sibling of `content` and ignores its shape. The block-list half is what
    /// fails before the change.
    #[test]
    fn tool_result_block_list_still_takes_cache_control() {
        let messages = history_with_tool_result(&format!(
            "shot [IMAGE:data:image/png;base64,{CANONICAL_PNG_B64}]"
        ));

        let (_, mut native_msgs) = AnthropicModelProvider::convert_messages(&messages);
        assert!(
            AnthropicModelProvider::should_cache_conversation(&messages),
            "this history must be long enough to be cached, or the test is vacuous"
        );
        AnthropicModelProvider::apply_cache_to_last_message(&mut native_msgs);

        let tool_result = first_tool_result_on_the_wire(&native_msgs);
        let blocks = tool_result["content"]
            .as_array()
            .unwrap_or_else(|| panic!("expected a block list: {tool_result}"));
        assert!(
            blocks.iter().any(|block| block["type"] == "image"),
            "expected an image inside the tool result: {tool_result}"
        );
        assert_eq!(
            tool_result["cache_control"]["type"], "ephemeral",
            "block-list content must not cost the request its cache breakpoint: {tool_result}"
        );
    }

    /// A non-JSON tool message that still sits inside the run following an
    /// assistant turn with exactly one unanswered `tool_use` becomes a real
    /// `tool_result` carrying that id, with the image nested inside it.
    ///
    /// Both halves fail before the change. The arm emitted top-level user text
    /// with the payload stripped, and because the message held no `tool_result`,
    /// `backfill_orphaned_tool_uses` inserted a "tool result missing" stub right
    /// beside the real result.
    #[test]
    fn non_json_tool_carrier_recovers_tool_use_id() {
        let messages = vec![
            ChatMessage::user("take a screenshot"),
            ChatMessage::assistant(
                serde_json::json!({
                    "content": "",
                    "tool_calls": [
                        {"id": "toolu_only", "name": "screenshot", "arguments": "{}"}
                    ]
                })
                .to_string(),
            ),
            ChatMessage::tool(format!(
                "raw output [IMAGE:data:image/png;base64,{CANONICAL_PNG_B64}]"
            )),
        ];

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);

        let tool_results = tool_results_on_the_wire(&native_msgs);
        assert_eq!(
            tool_results.len(),
            1,
            "the recovered result must be the only one — a stub beside it is the \
             bug this fixes: {tool_results:?}"
        );
        assert_eq!(tool_results[0]["tool_use_id"], "toolu_only");

        let blocks = tool_results[0]["content"]
            .as_array()
            .unwrap_or_else(|| panic!("expected a block list: {}", tool_results[0]));
        assert!(
            blocks
                .iter()
                .any(|block| block["type"] == "image"
                    && block["source"]["data"] == CANONICAL_PNG_B64),
            "the image must be nested in the recovered tool_result: {}",
            tool_results[0]
        );
        assert!(
            blocks.iter().any(|block| block["type"] == "text"
                && block["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("raw output"))),
            "the surrounding prose must survive: {}",
            tool_results[0]
        );

        let wire = serde_json::to_string(&native_msgs).expect("serialize");
        assert!(
            !wire.contains("tool result missing"),
            "recovery must stop the bogus stub: {wire}"
        );
    }

    /// With no single unanswered `tool_use` to pair against, the output degrades
    /// to top-level blocks: the image is still delivered, a text block always
    /// follows it, and no `tool_use_id` is invented.
    ///
    /// The image-block assertion is the discriminator. Before the change the
    /// payload was stripped and replaced by a note, so nothing was delivered.
    /// "No invented id" passes trivially today, because there was no recovery
    /// mechanism at all.
    #[test]
    fn non_json_tool_carrier_ambiguous_uses_top_level_blocks() {
        // Two unanswered calls, and a single unanswered call with no assistant
        // turn at all: both are ambiguous and must degrade the same way.
        let two_candidates = vec![
            ChatMessage::user("do two things"),
            ChatMessage::assistant(
                serde_json::json!({
                    "content": "",
                    "tool_calls": [
                        {"id": "toolu_a", "name": "shell", "arguments": "{}"},
                        {"id": "toolu_b", "name": "shell", "arguments": "{}"}
                    ]
                })
                .to_string(),
            ),
            ChatMessage::tool(format!(
                "raw output [IMAGE:data:image/png;base64,{CANONICAL_PNG_B64}]"
            )),
        ];
        let no_candidates = vec![
            ChatMessage::user("look"),
            ChatMessage::tool(format!(
                "raw output [IMAGE:data:image/png;base64,{CANONICAL_PNG_B64}]"
            )),
        ];

        for (label, messages) in [
            ("two unanswered tool_use blocks", two_candidates),
            ("no assistant turn at all", no_candidates),
        ] {
            let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);
            let blocks = last_user_blocks(&native_msgs);

            let image_at = block_position(&blocks, "image")
                .unwrap_or_else(|| panic!("{label}: expected a top-level image block: {blocks:?}"));
            assert_eq!(
                blocks[image_at]["source"]["data"], CANONICAL_PNG_B64,
                "{label}: the payload must be delivered, not dropped"
            );
            assert!(
                blocks[image_at + 1..]
                    .iter()
                    .any(|block| block["type"] == "text"),
                "{label}: a text block must follow the images, or the request \
                 silently loses its cache breakpoint: {blocks:?}"
            );
            assert_eq!(
                blocks.last().expect("blocks present")["type"],
                "text",
                "{label}: the message must not end on an image block: {blocks:?}"
            );

            for tool_result in tool_results_on_the_wire(&native_msgs) {
                let id = tool_result["tool_use_id"].as_str().unwrap_or_default();
                assert!(
                    ["toolu_a", "toolu_b"].contains(&id),
                    "{label}: no tool_use_id may be invented, saw {id}"
                );
                assert!(
                    !tool_result.to_string().contains("raw output"),
                    "{label}: unpaired output must not be attached to a call: {tool_result}"
                );
            }
        }
    }

    /// `assistant(tool A) -> user("cancel") -> non-JSON tool output`. The
    /// intervening user message ends the tool-result run, so the output is not
    /// paired with call A — but the image is still delivered as a top-level
    /// block.
    ///
    /// The negative half passes trivially before the change (there was no
    /// recovery to mispair with), so the delivery half is what makes this fail.
    /// Its counterpart is `non_json_tool_carrier_recovers_tool_use_id`, which is
    /// the same sequence without the intervening user message.
    #[test]
    fn non_json_tool_carrier_is_not_paired_across_a_user_turn() {
        let messages = vec![
            ChatMessage::user("go"),
            ChatMessage::assistant(
                serde_json::json!({
                    "content": "",
                    "tool_calls": [{"id": "toolu_a", "name": "shell", "arguments": "{}"}]
                })
                .to_string(),
            ),
            ChatMessage::user("cancel"),
            ChatMessage::tool(format!(
                "raw output [IMAGE:data:image/png;base64,{CANONICAL_PNG_B64}]"
            )),
        ];

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);

        let tool_results = tool_results_on_the_wire(&native_msgs);
        assert_eq!(
            tool_results.len(),
            1,
            "only the orphan stub: {tool_results:?}"
        );
        assert_eq!(tool_results[0]["tool_use_id"], "toolu_a");
        assert!(
            tool_results[0]["content"]
                .as_str()
                .is_some_and(|text| text.contains("tool result missing")),
            "call A must stay unanswered, not be paired with stale output: {}",
            tool_results[0]
        );

        let blocks = last_user_blocks(&native_msgs);
        let image_at = block_position(&blocks, "image")
            .unwrap_or_else(|| panic!("the image must still be delivered: {blocks:?}"));
        assert_eq!(blocks[image_at]["source"]["data"], CANONICAL_PNG_B64);
        assert_eq!(
            blocks.last().expect("blocks present")["type"],
            "text",
            "the message must not end on an image block: {blocks:?}"
        );
    }

    /// A tool-result envelope whose `tool_call_id` is `null` is not a shape the
    /// current turn engine emits — every native call carries an id all the way
    /// through `append_tool_round_to_history`. It is reachable from restored or
    /// externally supplied history and from the public `ChatMessage::tool`
    /// constructor, so the adapter must handle it: it takes the non-JSON carrier
    /// arm and still delivers the image.
    ///
    /// Delivery is what fails before the change. The "no base64 in the envelope
    /// text" half already held, because the payload was stripped.
    ///
    /// The envelope scaffolding must not reach the model either: an earlier
    /// version of this arm passed the whole raw message down, so the recovered
    /// `tool_result` read `{"tool_call_id":null,"content":"shot "}` as if the
    /// tool had written that JSON itself.
    #[test]
    fn null_id_envelope_still_delivers_the_image() {
        let envelope = serde_json::json!({
            "tool_call_id": serde_json::Value::Null,
            "content": format!("shot [IMAGE:data:image/png;base64,{CANONICAL_PNG_B64}]"),
        })
        .to_string();
        let messages = vec![
            ChatMessage::user("screenshot please"),
            ChatMessage::assistant(
                serde_json::json!({
                    "content": "",
                    "tool_calls": [
                        {"id": "toolu_shot", "name": "screenshot", "arguments": "{}"}
                    ]
                })
                .to_string(),
            ),
            ChatMessage::tool(envelope),
        ];

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);

        let tool_results = tool_results_on_the_wire(&native_msgs);
        assert_eq!(tool_results.len(), 1, "{tool_results:?}");
        assert_eq!(
            tool_results[0]["tool_use_id"], "toolu_shot",
            "the id is recovered from the assistant turn, not from the envelope"
        );
        let blocks = tool_results[0]["content"]
            .as_array()
            .unwrap_or_else(|| panic!("expected a block list: {}", tool_results[0]));
        assert!(
            blocks
                .iter()
                .any(|block| block["type"] == "image"
                    && block["source"]["data"] == CANONICAL_PNG_B64),
            "the image must be delivered: {}",
            tool_results[0]
        );

        assert!(
            blocks
                .iter()
                .any(|block| block["type"] == "text" && block["text"].as_str() == Some("shot")),
            "the tool's own output must survive without the envelope around it: {}",
            tool_results[0]
        );

        let wire = serde_json::to_value(&native_msgs).expect("serialize");
        let mut texts = Vec::new();
        text_fields(&wire, &mut texts);
        assert!(
            texts.iter().all(|text| !text.contains(CANONICAL_PNG_B64)),
            "the payload must never sit in a text position: {texts:?}"
        );
        assert!(
            texts.iter().all(|text| !text.contains("tool_call_id")),
            "the envelope scaffolding must not be handed to the model as prose: {texts:?}"
        );
    }

    /// One `tool_use` answered twice in the same merged user message collapses to
    /// a single `tool_result`, and the loser's output is still delivered.
    ///
    /// The non-JSON carrier recovers the only outstanding id, marks it answered,
    /// and a JSON envelope naming that same id then merges into the same user
    /// message. Two `tool_result` blocks with one id is a 400 — the exact class of
    /// failure `backfill_orphaned_tool_uses` exists to prevent — and id recovery
    /// introduced it, so it is fixed here rather than in the recovery rule, which
    /// cannot see a duplicate that arrives later.
    #[test]
    fn duplicate_tool_result_ids_are_collapsed_in_one_message() {
        let messages = vec![
            ChatMessage::user("go"),
            ChatMessage::assistant(
                serde_json::json!({
                    "content": "",
                    "tool_calls": [{"id": "toolu_a", "name": "screenshot", "arguments": "{}"}]
                })
                .to_string(),
            ),
            // Non-JSON: recovery pairs this with toolu_a.
            ChatMessage::tool(format!(
                "raw output [IMAGE:data:image/png;base64,{CANONICAL_PNG_B64}]"
            )),
            // An envelope for the same call, from restored or externally supplied
            // history.
            ChatMessage::tool(
                serde_json::json!({"tool_call_id": "toolu_a", "content": "second answer"})
                    .to_string(),
            ),
        ];

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);

        let tool_results = tool_results_on_the_wire(&native_msgs);
        assert_eq!(
            tool_results.len(),
            1,
            "a tool_use may only be answered once per message: {tool_results:?}"
        );
        assert_eq!(tool_results[0]["tool_use_id"], "toolu_a");

        let blocks = last_user_blocks(&native_msgs);
        let wire = serde_json::to_string(&native_msgs).expect("serialize");
        assert!(
            wire.contains("raw output"),
            "the first answer must stay in the tool_result: {wire}"
        );
        assert!(
            wire.contains("second answer"),
            "the demoted answer must still reach the model: {wire}"
        );
        assert!(
            wire.contains("[duplicate result for tool call toolu_a]"),
            "demoted output must say it is a tool's second answer, not user prose: {wire}"
        );
        assert_eq!(
            blocks.last().expect("blocks present")["type"],
            "text",
            "the message must not end on an image block: {blocks:?}"
        );
    }

    /// A `tool_use` id is still recovered across a system message.
    ///
    /// A system message is not emitted into the message list, so it cannot break
    /// the `tool_use`-to-`tool_result` adjacency on the wire and does not end the
    /// tool-result run. Pinned because a user message and a plain assistant
    /// message both do end the run, and the difference is deliberate.
    #[test]
    fn system_message_does_not_end_the_tool_result_run() {
        let messages = vec![
            ChatMessage::user("go"),
            ChatMessage::assistant(
                serde_json::json!({
                    "content": "",
                    "tool_calls": [{"id": "toolu_a", "name": "screenshot", "arguments": "{}"}]
                })
                .to_string(),
            ),
            ChatMessage::system("You take screenshots."),
            ChatMessage::tool(format!(
                "raw output [IMAGE:data:image/png;base64,{CANONICAL_PNG_B64}]"
            )),
        ];

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);

        let tool_results = tool_results_on_the_wire(&native_msgs);
        assert_eq!(tool_results.len(), 1, "{tool_results:?}");
        assert_eq!(
            tool_results[0]["tool_use_id"], "toolu_a",
            "an intervening system message must not block recovery"
        );
        let wire = serde_json::to_string(&native_msgs).expect("serialize");
        assert!(
            !wire.contains("tool result missing"),
            "no stub may sit beside the real result: {wire}"
        );
    }

    /// A line-wrapped unterminated marker leaves no base64 behind either, and
    /// ordinary prose after a data URI survives.
    ///
    /// `parse_image_markers` only collapses a wrapped marker when it is
    /// terminated, so a truncated wrapped payload arrives with its newlines
    /// intact. Sweeping only to the first newline left every later line in a text
    /// position — tens of thousands of prose tokens, which is the original bug.
    #[test]
    fn wrapped_unterminated_marker_leaves_no_base64_in_text() {
        // Two lines, each a full canonical payload, with no closing bracket.
        let wrapped =
            format!("[IMAGE:data:image/png;base64,{CANONICAL_PNG_B64}\n{CANONICAL_PNG_B64}");
        let messages = history_with_tool_result(&format!("saved {wrapped}"));

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);
        let wire = serde_json::to_string(&native_msgs).expect("serialize");
        assert!(
            !wire.contains(CANONICAL_PNG_B64),
            "no wrapped line may survive on the wire: {wire}"
        );
        assert!(
            wire.contains("[truncated inline data removed]"),
            "the replacement literal must say what happened: {wire}"
        );

        // The continuation rule must not eat prose: a short word after the
        // payload is not a wrapped line.
        let with_prose = history_with_tool_result(&format!(
            "[IMAGE:data:image/png;base64,{CANONICAL_PNG_B64}\nthe screenshot was truncated"
        ));
        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&with_prose);
        let wire = serde_json::to_string(&native_msgs).expect("serialize");
        assert!(
            wire.contains("the screenshot was truncated"),
            "prose after a swept run must survive: {wire}"
        );
    }

    /// The residual sweep makes one pass over its input, and says so by the
    /// clock rather than by hanging.
    ///
    /// Tool output is untrusted. An earlier version restarted a search for
    /// `;base64,` from every rejected `data:`, which is quadratic: this input took
    /// tens of seconds of CPU inside `convert_messages`, on every turn for as long
    /// as the message stayed in history.
    ///
    /// Two things are pinned deliberately. The repeat count is **odd**, so the
    /// examined `data:` positions do not land on the real header by arithmetic
    /// accident — with an even count they do, and the earlier version of this test
    /// passed even while the sweep could be bypassed entirely. And the elapsed
    /// time is asserted, so a quadratic regression names itself instead of
    /// stalling the whole suite with no attribution. The bound is three orders of
    /// magnitude above the measured cost (tens of milliseconds) and three orders
    /// below the quadratic version, so a loaded CI machine cannot flake it.
    #[test]
    fn residual_sweep_stays_linear_on_repeated_data_prefixes() {
        let mut text = "data:".repeat(100_001);
        text.push_str("data:image/png;base64,AAAA");

        let started = std::time::Instant::now();
        let swept = AnthropicModelProvider::sweep_residual_image_data(&text);
        let elapsed = started.elapsed();

        assert!(
            swept.ends_with(TRUNCATED_DATA_NOTE),
            "the real run at the end must still be swept"
        );
        assert!(
            !swept.contains(";base64,"),
            "no header may survive the sweep"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "the sweep must stay linear; took {elapsed:?} on {} bytes",
            text.len()
        );
    }

    /// A `data:` that starts inside the *payload* of the run being swept is
    /// still examined.
    ///
    /// The letters of `data` are all in the base64 alphabet, so a payload run
    /// swallowed them and stopped at the `:` of the next scheme. Resuming the
    /// scan at that colon meant the overlapping `data:` was never seen, and
    /// `[IMAGE:data:image/png;base64,AAAAdata:image/png;base64,<payload>` came
    /// back with `:image/png;base64,<payload>` still in a text position. Same
    /// class of hole as [`Self::nested_data_prefix_does_not_bypass_the_sweep`],
    /// at the other boundary of the run.
    #[test]
    fn payload_boundary_data_prefix_does_not_bypass_the_sweep() {
        for (label, text) in [
            (
                "overlap after base64-legal payload bytes",
                format!(
                    "[IMAGE:data:image/png;base64,AAAAdata:image/png;base64,{CANONICAL_PNG_B64}"
                ),
            ),
            (
                "overlap with an empty payload before it",
                format!("data:image/png;base64,data:image/png;base64,{CANONICAL_PNG_B64}"),
            ),
            (
                "two overlaps in a row",
                format!(
                    "data:image/png;base64,AAdata:image/png;base64,AAdata:image/png;base64,{CANONICAL_PNG_B64}"
                ),
            ),
        ] {
            let swept = AnthropicModelProvider::sweep_residual_image_data(&text);
            assert!(
                !swept.contains(CANONICAL_PNG_B64),
                "{label}: the payload must not survive: {swept}"
            );
            assert!(
                !swept.contains(";base64,"),
                "{label}: no header may survive either: {swept}"
            );
        }
    }

    /// The payload of an overlapping run reaches no text field on the wire.
    ///
    /// The unit test above pins the sweep itself; this pins the property a
    /// reader actually cares about, through the whole conversion.
    #[test]
    fn overlapping_marker_payload_reaches_no_serialized_text_field() {
        let overlapped =
            format!("[IMAGE:data:image/png;base64,AAAAdata:image/png;base64,{CANONICAL_PNG_B64}");
        let messages = history_with_tool_result(&format!("saved {overlapped}"));

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);
        let wire = serde_json::to_string(&native_msgs).expect("serialize");

        assert!(
            !wire.contains(CANONICAL_PNG_B64),
            "the overlapped payload must not survive anywhere on the wire: {wire}"
        );
        assert!(
            wire.contains("[truncated inline data removed]"),
            "the replacement literal must say what happened: {wire}"
        );
    }

    /// A near-miss `base64` parameter is refused by the splitter, so it cannot
    /// be delivered as an image while the sweep declines to sweep it.
    ///
    /// The splitter accepted any header *containing* `;base64`, while the sweep
    /// requires an exact `base64` parameter. A truncated `;base64foo` header
    /// therefore fell between them: no reference to deliver, and no sweep.
    #[test]
    fn near_miss_base64_parameter_is_refused_by_the_splitter() {
        for header in [";base64foo", ";base64=1", ";xbase64"] {
            let candidate = format!("data:image/png{header},{CANONICAL_PNG_B64}");
            assert!(
                crate::multimodal::split_base64_image_data_uri(&candidate, 10 * 1024 * 1024)
                    .is_err(),
                "{header}: a header without an exact base64 parameter is not a base64 data URI"
            );
        }

        // The forms the sweep does accept must still split, including `base64`
        // ahead of another parameter.
        for header in [";base64", ";base64;charset=x", ";charset=x;base64"] {
            let candidate = format!("data:image/png{header},{CANONICAL_PNG_B64}");
            assert!(
                crate::multimodal::split_base64_image_data_uri(&candidate, 10 * 1024 * 1024)
                    .is_ok(),
                "{header}: an exact base64 parameter must still be accepted"
            );
        }
    }

    /// A `data:` that starts inside another `data:` header is still examined.
    ///
    /// The four letters of `data` are legal header characters and only the `:`
    /// stops the header walk, so resuming the search at the end of a rejected
    /// header jumped straight over the nested occurrence. That let
    /// `data:data:image/png;base64,<payload>` through with the payload intact —
    /// the one thing the sweep exists to prevent, defeated by five extra
    /// characters of untrusted tool output.
    #[test]
    fn nested_data_prefix_does_not_bypass_the_sweep() {
        for (label, text) in [
            (
                "nested scheme",
                format!("data:data:image/png;base64,{CANONICAL_PNG_B64}"),
            ),
            (
                "rejected header holding the real one",
                format!("xdata:image/pngdata:image/png;base64,{CANONICAL_PNG_B64}"),
            ),
            (
                "three deep",
                format!("data:data:data:image/png;base64,{CANONICAL_PNG_B64}"),
            ),
        ] {
            let swept = AnthropicModelProvider::sweep_residual_image_data(&text);
            assert!(
                !swept.contains(CANONICAL_PNG_B64),
                "{label}: the payload must not survive: {swept}"
            );
            assert!(
                swept.contains(TRUNCATED_DATA_NOTE),
                "{label}: the replacement literal must say what happened: {swept}"
            );
        }
    }

    /// A truncated wrapped payload is swept at every wrap width, not only at 64
    /// columns and wider.
    ///
    /// The rule keys on uniform line width, so it does not care what the width
    /// is. An earlier version needed each continued line to hold at least 64
    /// base64 characters, which left a payload wrapped at 40, 56, 60 or 63
    /// columns almost entirely in a text position — the bug it was written to
    /// fix. Ruby's `Base64.encode64` wraps at 60.
    #[test]
    fn wrapped_payload_is_swept_at_every_wrap_width() {
        for width in [40usize, 56, 60, 63, 64, 76] {
            // The whole marker text wrapped at `width`, the shape a producer that
            // hard-wraps its output emits: every line is exactly `width` wide.
            // `Z` appears in neither the marker prefix nor the replacement
            // literal, so counting it counts payload characters only.
            let mut raw = format!("[IMAGE:data:image/png;base64,{}", "Z".repeat(4_000));
            let mut wrapped = String::new();
            while raw.len() > width {
                let line: String = raw.drain(..width).collect();
                wrapped.push_str(&line);
                wrapped.push('\n');
            }
            let tail_len = raw.len();
            wrapped.push_str(&raw);

            let swept = AnthropicModelProvider::sweep_residual_image_data(&wrapped);

            // Only the last, shorter line may survive: it is not a wrap-width
            // line, and absorbing it would mean deleting whatever short word
            // follows a quoted data URI.
            let left = swept.chars().filter(|ch| *ch == 'Z').count();
            assert!(
                left <= tail_len,
                "width {width}: {left} base64 characters left in text, at most {tail_len} allowed: {swept}"
            );
            assert!(
                swept.contains(TRUNCATED_DATA_NOTE),
                "width {width}: the replacement literal must be present: {swept}"
            );
        }
    }

    /// Real tool output after a quoted data URI is not swallowed by the
    /// continuation rule.
    ///
    /// A sha256 digest is exactly 64 characters of base64-alphabet text, so an
    /// earlier rule that continued a run across any whitespace into any segment
    /// of 64 or more such characters silently deleted digest listings, hex id
    /// columns and PEM bodies that happened to follow a data URI. The
    /// continuation now needs uniform line width, and none of these has it.
    #[test]
    fn output_after_a_quoted_data_uri_survives_the_sweep() {
        let digest_a = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let digest_b = "d2a84f4b8b650937ec8f73cd8be2c74add5a911ba64df27458ed8229da804a26";
        for (label, text, must_survive) in [
            (
                "sha256sum listing",
                format!("data:image/png;base64,AAAA\n{digest_a}  a.png\n{digest_b}  b.png"),
                vec![digest_a, digest_b, "a.png", "b.png"],
            ),
            (
                "single digest then prose",
                format!("icon: data:image/png;base64,AAAA\n{digest_a}\nDone."),
                vec![digest_a, "Done."],
            ),
            (
                "a long token after one space",
                format!("data:image/png;base64,AAAA {digest_a}"),
                vec![digest_a],
            ),
            (
                "uniform id column",
                format!("data:image/png;base64,AAAA\n{digest_a}\n{digest_b}\n{digest_a}\nsummary"),
                vec![digest_a, digest_b, "summary"],
            ),
            (
                // Equal-length lines, but four characters is not a wrap width.
                "uniform column of short codes",
                "data:image/png;base64,AAAA\nQ4XZ\nP7KM\nR2VB\ndone".to_string(),
                vec!["Q4XZ", "P7KM", "R2VB", "done"],
            ),
        ] {
            let swept = AnthropicModelProvider::sweep_residual_image_data(&text);
            for expected in must_survive {
                assert!(
                    swept.contains(expected),
                    "{label}: {expected} was real tool output and must survive: {swept}"
                );
            }
            assert!(
                swept.contains(TRUNCATED_DATA_NOTE),
                "{label}: the data URI itself is still swept: {swept}"
            );
        }
    }

    /// The sweep's replacement literal does not claim an image was removed.
    ///
    /// The header rule accepts any media type, deliberately: any base64 blob in a
    /// text position is the token blowup the sweep exists to stop. Saying "image"
    /// would then be false for a JSON or text data URI — the same class of defect
    /// as the old omission note that told the model Anthropic cannot carry images.
    #[test]
    fn sweep_note_does_not_claim_an_image_for_other_media_types() {
        let swept = AnthropicModelProvider::sweep_residual_image_data(
            "config blob: data:application/json;base64,eyJhIjoxfQ== (decode it)",
        );

        assert_eq!(
            swept, "config blob: [truncated inline data removed] (decode it)",
            "the note must not name a media type it did not see"
        );
    }

    /// `;base64` is recognised anywhere in the header's parameter list.
    ///
    /// `crate::multimodal::split_base64_image_data_uri` accepts it in any
    /// position, so a terminated `data:image/png;base64;charset=x,<payload>`
    /// marker is delivered as an image. Requiring it last here left the same
    /// header unswept when the marker was truncated, so the payload was billed as
    /// prose.
    #[test]
    fn sweep_accepts_base64_before_other_header_parameters() {
        let text = format!("data:image/png;base64;charset=utf-8,{CANONICAL_PNG_B64} tail");
        let swept = AnthropicModelProvider::sweep_residual_image_data(&text);

        assert!(
            !swept.contains(CANONICAL_PNG_B64),
            "the payload must not survive: {swept}"
        );
        assert!(swept.ends_with(" tail"), "prose must survive: {swept}");
    }

    /// A tool result whose `content` is structured JSON still reaches the model.
    ///
    /// Only a string `content` was read, so an envelope carrying an object or an
    /// array emitted an empty `tool_result` with nothing on the wire saying the
    /// output had been dropped — and on the unusable-id branch it handed the whole
    /// envelope to the model as if the tool had written the scaffolding.
    #[test]
    fn envelope_with_structured_content_still_delivers_the_output() {
        for (label, envelope) in [
            (
                "usable id",
                serde_json::json!({"tool_call_id": "toolu_a", "content": {"rows": 2}}),
            ),
            (
                "null id",
                serde_json::json!({"tool_call_id": null, "content": {"rows": 2}}),
            ),
            (
                "numeric id",
                serde_json::json!({"tool_call_id": 7, "content": ["a", "b"]}),
            ),
        ] {
            let messages = vec![
                ChatMessage::user("go"),
                ChatMessage::assistant(
                    serde_json::json!({
                        "content": "",
                        "tool_calls": [{"id": "toolu_a", "name": "query", "arguments": "{}"}]
                    })
                    .to_string(),
                ),
                ChatMessage::tool(envelope.to_string()),
            ];

            let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);
            let wire = serde_json::to_value(&native_msgs).expect("serialize");
            let mut texts = Vec::new();
            text_fields(&wire, &mut texts);
            let flat = serde_json::to_string(&wire).expect("serialize");

            assert!(
                flat.contains("rows") || flat.contains(r#"\"a\""#),
                "{label}: the tool's own output must reach the model: {flat}"
            );
            assert!(
                texts.iter().all(|text| !text.contains("tool_call_id")),
                "{label}: the envelope scaffolding must stay off the wire: {texts:?}"
            );
        }
    }

    /// An envelope with an unusable id and no `content` key is dropped, not
    /// printed.
    ///
    /// There is no payload to keep, so the only alternative was handing
    /// `{"tool_call_id":null}` to the model as the tool's output. The call is left
    /// to `backfill_orphaned_tool_uses`, which says plainly that the result is
    /// missing.
    #[test]
    fn envelope_with_no_content_and_no_usable_id_is_dropped() {
        let messages = vec![
            ChatMessage::user("go"),
            ChatMessage::assistant(
                serde_json::json!({
                    "content": "",
                    "tool_calls": [{"id": "toolu_a", "name": "query", "arguments": "{}"}]
                })
                .to_string(),
            ),
            ChatMessage::tool(serde_json::json!({"tool_call_id": null}).to_string()),
        ];

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);
        let wire = serde_json::to_string(&native_msgs).expect("serialize");

        assert!(
            !wire.contains("tool_call_id"),
            "the envelope scaffolding must not be handed to the model: {wire}"
        );
        assert!(
            wire.contains("tool result missing"),
            "the unanswered call must still be backfilled: {wire}"
        );
    }

    /// A user-message reference that is neither a deliverable data URI nor a
    /// readable local file produces no `image` block, is counted in the omission
    /// note, and never makes the converter fetch anything.
    ///
    /// Pins the two branches the stricter validation left untested: a local path
    /// that does not exist, and an `http` URL, which `parse_image_markers` returns
    /// as a reference. Both are reported the same way the tool arms report them —
    /// the converter does no network I/O, so an unfetched URL is simply not
    /// deliverable from here.
    #[test]
    fn unloadable_user_message_references_are_counted_not_sent() {
        for (label, reference) in [
            ("missing local path", "/definitely/not/here.png"),
            ("remote url", "http://example.com/a.png"),
        ] {
            let messages = vec![ChatMessage::user(format!("look at [IMAGE:{reference}]"))];

            let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);
            let blocks = last_user_blocks(&native_msgs);

            assert!(
                block_position(&blocks, "image").is_none(),
                "{label}: nothing deliverable, so no image block: {blocks:?}"
            );
            let wire = serde_json::to_string(&native_msgs).expect("serialize");
            assert!(
                wire.contains(OMISSION_NOTE_ONE),
                "{label}: the drop must be visible to the model: {wire}"
            );
            assert!(
                !wire.contains("\"[image]\""),
                "{label}: no placeholder may claim an image is attached: {wire}"
            );
        }
    }

    /// Every `tool_result` block precedes every other block in a merged user
    /// message, and the orphan backfill still lands in front of them.
    ///
    /// Anthropic returns a 400 when text precedes a `tool_result` in the same
    /// message, and this history produces exactly that on the unmodified branch:
    /// the ambiguous non-JSON output becomes top-level blocks and the JSON
    /// envelope that follows appends a `tool_result` after them.
    ///
    /// Two unanswered `tool_use` blocks make the recovery in step 4 ambiguous on
    /// purpose. With a single call the id would be recovered, the top-level text
    /// block would disappear, and the ordering assertion would be vacuous.
    #[test]
    fn tool_results_precede_other_blocks_in_a_merged_user_message() {
        let messages = vec![
            ChatMessage::user("do two things"),
            ChatMessage::assistant(
                serde_json::json!({
                    "content": "",
                    "tool_calls": [
                        {"id": "toolu_a", "name": "shell", "arguments": "{}"},
                        {"id": "toolu_b", "name": "shell", "arguments": "{}"}
                    ]
                })
                .to_string(),
            ),
            // Ambiguous: two candidates, so this stays top-level text + image.
            ChatMessage::tool(format!(
                "raw output [IMAGE:data:image/png;base64,{CANONICAL_PNG_B64}]"
            )),
            // Answers A. B stays orphaned, so the backfill has work to do.
            ChatMessage::tool(
                serde_json::json!({"tool_call_id": "toolu_a", "content": "done"}).to_string(),
            ),
        ];

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);
        let blocks = last_user_blocks(&native_msgs);

        let last_tool_result = blocks
            .iter()
            .rposition(|block| block["type"] == "tool_result")
            .unwrap_or_else(|| panic!("expected a tool_result: {blocks:?}"));
        assert!(
            blocks[..last_tool_result]
                .iter()
                .all(|block| block["type"] == "tool_result"),
            "no block may precede a tool_result in a merged user message: {blocks:?}"
        );

        let ids: Vec<&str> = blocks[..=last_tool_result]
            .iter()
            .map(|block| block["tool_use_id"].as_str().unwrap_or_default())
            .collect();
        assert!(
            ids.contains(&"toolu_a") && ids.contains(&"toolu_b"),
            "the orphan backfill must still answer B: {blocks:?}"
        );

        // The ordering pass must not push the image into last position, where
        // `apply_cache_to_last_message` silently attaches nothing.
        assert_eq!(
            blocks.last().expect("blocks present")["type"],
            "text",
            "the message must not end on an image block: {blocks:?}"
        );
        assert!(
            blocks
                .iter()
                .any(|block| block["type"] == "image"
                    && block["source"]["data"] == CANONICAL_PNG_B64),
            "the image must still be delivered: {blocks:?}"
        );
    }

    /// A marker with no closing `]` leaves raw base64 in a text position:
    /// `parse_image_markers` copies the remainder verbatim into the cleaned text
    /// and returns no reference, so the zero-reference early return passed it
    /// straight through. Asserted on both a tool result and a user message,
    /// because both consume the same parser.
    ///
    /// Fails before the change on both arms: the payload survives in a `text`
    /// field and no replacement literal is written.
    #[test]
    fn unterminated_marker_leaves_no_base64_in_text() {
        let unterminated = format!("[IMAGE:data:image/png;base64,{CANONICAL_PNG_B64}");

        let tool_messages = history_with_tool_result(&format!("saved {unterminated}"));
        let user_messages = vec![ChatMessage::user(format!("look at {unterminated}"))];

        for (label, messages) in [
            ("tool result", tool_messages),
            ("user message", user_messages),
        ] {
            let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);
            // The whole serialized request, not just `text` fields: an
            // image-free tool result carries its prose as a bare JSON string on
            // `content`, which is a text position all the same.
            let wire = serde_json::to_string(&native_msgs).expect("serialize");

            assert!(
                !wire.contains(CANONICAL_PNG_B64),
                "{label}: raw base64 must not survive anywhere on the wire: {wire}"
            );
            assert!(
                wire.contains("[truncated inline data removed]"),
                "{label}: the replacement literal must say what happened: {wire}"
            );
            // A truncated marker was never a reference, so it is not counted.
            assert!(
                !wire.contains("image(s) omitted"),
                "{label}: a swept run must not be double-reported as an omission: {wire}"
            );
        }
    }

    /// The user arm now runs its data URIs through the same structural check the
    /// tool arms use. Each rejection class carries a deliverable PNG alongside
    /// it, and the all-rejected case must not claim an image is attached.
    ///
    /// Nothing else exercises the user arm's validation: the tool-arm rejection
    /// test asserts a note only the tool path wrote, and
    /// `user_message_images_still_become_image_blocks` uses a payload both the
    /// old split and the new check accept. Before the change the old code split
    /// on the first comma and trusted whatever came out, so every rejected
    /// reference below became an `image` block on the wire.
    #[test]
    fn rejected_user_message_data_uris_produce_no_image_block() {
        let oversized = "A".repeat(MAX_ENCODED_IMAGE_PAYLOAD_BYTES + 4);
        let cases: Vec<(&str, String)> = vec![
            (
                "header does not declare ;base64",
                format!("data:image/png,{CANONICAL_JPEG_B64}"),
            ),
            (
                "media type outside the allowlist",
                "data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=".to_string(),
            ),
            (
                "payload length is not a multiple of four",
                "data:image/gif;base64,R0lGODlhAQABAA".to_string(),
            ),
            (
                "payload over the encoded ceiling",
                format!("data:image/png;base64,{oversized}"),
            ),
        ];

        for (label, rejected) in cases {
            let messages = vec![ChatMessage::user(format!(
                "prose [IMAGE:{rejected}] [IMAGE:data:image/png;base64,{CANONICAL_PNG_B64}]"
            ))];

            let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);
            let blocks = last_user_blocks(&native_msgs);

            let images: Vec<&serde_json::Value> = blocks
                .iter()
                .filter(|block| block["type"] == "image")
                .collect();
            assert_eq!(
                images.len(),
                1,
                "{label}: only the valid sibling may become an image block: {blocks:?}"
            );
            assert_eq!(images[0]["source"]["data"], CANONICAL_PNG_B64, "{label}");

            let text = blocks
                .iter()
                .find(|block| block["type"] == "text")
                .and_then(|block| block["text"].as_str())
                .unwrap_or_else(|| panic!("{label}: expected a text block: {blocks:?}"));
            assert!(
                text.contains(OMISSION_NOTE_ONE),
                "{label}: the rejection must be visible to the model, got {text}"
            );

            let wire = serde_json::to_string(&native_msgs).expect("serialize");
            let rejected_payload = rejected
                .rsplit(',')
                .next()
                .expect("data URI payload after the comma");
            assert!(
                !wire.contains(rejected_payload),
                "{label}: a rejected payload must not reach the wire"
            );
        }

        // A user message whose only reference is rejected must not tell the
        // model an image is attached with nothing on the wire.
        let only_rejected = vec![ChatMessage::user(
            "[IMAGE:data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=]",
        )];
        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&only_rejected);
        let blocks = last_user_blocks(&native_msgs);

        assert!(
            block_position(&blocks, "image").is_none(),
            "a rejected reference must not produce an image block: {blocks:?}"
        );
        assert!(
            !blocks
                .iter()
                .any(|block| block["text"] == IMAGE_ONLY_TEXT_PLACEHOLDER),
            "the bare `[image]` placeholder must be gated on a block being \
             built, not on references existing: {blocks:?}"
        );
        assert!(
            blocks.iter().any(|block| block["text"]
                .as_str()
                .is_some_and(|text| text.contains(OMISSION_NOTE_ONE))),
            "an all-rejected user message must still say so: {blocks:?}"
        );
    }

    /// The composition test: a real PNG on disk, run through multimodal
    /// preparation and then through the converter, reaches the wire as a nested
    /// `image` block. Every other test here starts from an already-normalized
    /// data URI, which proves nothing about the join between the two halves, and
    /// the existing preparation tests stop at "the data URI is inside the
    /// tool-result JSON" without ever converting it.
    ///
    /// Fails before the change: preparation already produced the data URI, and
    /// the converter then stripped it and wrote an omission note.
    ///
    /// The tool message has to be last. `latest_tool_result_indices` only
    /// normalizes the trailing run of tool results; anywhere else the marker is
    /// replaced with `[image removed from history]` and this test asserts
    /// nothing.
    #[tokio::test]
    async fn prepared_local_image_reaches_the_wire_as_a_nested_block() {
        let temp = tempfile::tempdir().expect("temp dir");
        let image_path = temp.path().join("screenshot.png");
        // A PNG signature is enough for MIME detection, and its 12-character
        // base64 is canonical.
        std::fs::write(
            &image_path,
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
        )
        .expect("write png");

        let messages = vec![
            ChatMessage::user("take a screenshot"),
            ChatMessage::assistant(
                serde_json::json!({
                    "content": "",
                    "tool_calls": [
                        {"id": "toolu_shot", "name": "screenshot", "arguments": "{}"}
                    ]
                })
                .to_string(),
            ),
            ChatMessage::tool(
                serde_json::json!({
                    "tool_call_id": "toolu_shot",
                    // Drive-letter paths are loadable references, so this works
                    // on Windows as well as Unix.
                    "content": format!("saved [IMAGE:{}]", image_path.display()),
                })
                .to_string(),
            ),
        ];

        let prepared = crate::multimodal::prepare_messages_for_provider(
            &messages,
            &zeroclaw_config::schema::MultimodalConfig::default(),
        )
        .await
        .expect("preparation should succeed for a local PNG");
        assert!(
            prepared.contains_images,
            "preparation must have found the marker, or the rest asserts nothing"
        );

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&prepared.messages);
        let tool_result = first_tool_result_on_the_wire(&native_msgs);
        assert_eq!(tool_result["tool_use_id"], "toolu_shot");

        let blocks = tool_result["content"]
            .as_array()
            .unwrap_or_else(|| panic!("expected a block list: {tool_result}"));
        let image = blocks
            .iter()
            .find(|block| block["type"] == "image")
            .unwrap_or_else(|| panic!("no nested image block: {tool_result}"));
        assert_eq!(image["source"]["type"], "base64");
        assert_eq!(image["source"]["media_type"], "image/png");
        let data = image["source"]["data"]
            .as_str()
            .expect("base64 payload string");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .expect("payload must be decodable base64"),
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
            "the bytes written to disk must be the bytes on the wire"
        );

        let wire = serde_json::to_value(&native_msgs).expect("serialize");
        let mut texts = Vec::new();
        text_fields(&wire, &mut texts);
        assert!(
            texts.iter().all(|text| !text.contains(data)),
            "the payload must never sit in a text position: {texts:?}"
        );
        assert!(
            !wire.to_string().contains("image(s) omitted"),
            "a prepared local image must not be reported as omitted: {wire}"
        );
        assert!(
            !wire.to_string().contains("screenshot.png"),
            "the raw local path must not leak onto the wire: {wire}"
        );
    }

    /// Regression guard: ordinary user-message images take the user arm and
    /// must still become real image blocks. This fix must not touch them. The
    /// payload is canonical, which is why this pin survives the stricter
    /// validation and also why it cannot demonstrate it.
    #[test]
    fn user_message_images_still_become_image_blocks() {
        let messages = vec![ChatMessage::user(
            "what is this [IMAGE:data:image/jpeg;base64,/9j/4AAQ]",
        )];

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);

        let has_image = native_msgs
            .iter()
            .flat_map(|m| &m.content)
            .any(|block| matches!(block, NativeContentOut::Image { .. }));
        assert!(has_image, "user-message images must still be delivered");
    }

    /// The wire-shape pin for the two-shape content: an image-free tool result
    /// must still serialize `content` as a bare JSON string, byte-identically
    /// to before nested blocks existed.
    ///
    /// This passes today by design; it is not a proof of the fix. It has to
    /// assert on serialized JSON, because a value comparison against a Rust
    /// string passes for any serde encoding — including a tagged one that would
    /// put an object on the wire for every existing image-free tool result,
    /// which is exactly the regression this test exists to catch.
    #[test]
    fn image_free_tool_result_serializes_as_a_bare_string() {
        let tool_env = serde_json::json!({
            "tool_call_id": "toolu_ls",
            "content": "a.txt\nb.txt",
        })
        .to_string();
        let messages = vec![ChatMessage::user("list"), ChatMessage::tool(tool_env)];

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);

        let tool_result = first_tool_result_on_the_wire(&native_msgs);
        assert!(
            tool_result["content"].is_string(),
            "content must be a bare JSON string, not an object or a list: {tool_result}"
        );
        assert_eq!(tool_result["content"], "a.txt\nb.txt");

        let wire = serde_json::to_string(&native_msgs).expect("serialize");
        assert!(
            wire.contains(r#""content":"a.txt\nb.txt""#),
            "the string shape must serialize untagged: {wire}"
        );
    }

    /// Unloadable placeholders are prose, not payloads: they must survive
    /// untouched and must not be counted as omitted images.
    ///
    /// This passes today by design; it guards against the new block logic
    /// starting to count placeholders as omitted images.
    #[test]
    fn unloadable_image_placeholder_stays_literal_in_tool_result() {
        let tool_env = serde_json::json!({
            "tool_call_id": "toolu_doc",
            "content": "see [IMAGE:<path>] for details",
        })
        .to_string();
        let messages = vec![ChatMessage::user("doc"), ChatMessage::tool(tool_env)];

        let (_, native_msgs) = AnthropicModelProvider::convert_messages(&messages);

        let tool_result = first_tool_result_on_the_wire(&native_msgs);
        assert_eq!(tool_result["content"], "see [IMAGE:<path>] for details");
        assert!(
            !tool_result.to_string().contains("image(s) omitted"),
            "a placeholder is prose and must not be counted: {tool_result}"
        );
    }
}
