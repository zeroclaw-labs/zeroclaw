use crate::ModelProviderRuntimeOptions;
use crate::auth::AuthService;
use crate::auth::openai_oauth::extract_account_id_from_jwt;
use crate::multimodal;
use crate::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    ModelProvider, ProviderCapabilities, StreamChunk, StreamError, StreamEvent, StreamOptions,
    StreamResult, TokenUsage, ToolCall as ProviderToolCall,
};
use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use zeroclaw_api::tool::ToolSpec;

const DEFAULT_CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const DEFAULT_CODEX_INSTRUCTIONS: &str =
    "You are ZeroClaw, a concise and helpful coding assistant.";
/// OpenAI Codex speaks the "responses" wire protocol, not chat_completions.
const WIRE_API: &str = "responses";
const RESPONSES_HISTORY_PROVIDER: &str = "openai_codex";
const RESPONSES_HISTORY_KIND: &str = "responses_output_items";

#[derive(Clone)]
pub struct OpenAiCodexModelProvider {
    /// `[providers.models.<family>.<alias>]` config-key alias.
    alias: String,
    auth: AuthService,
    auth_profile_override: Option<String>,
    responses_url: String,
    custom_endpoint: bool,
    gateway_api_key: Option<String>,
    reasoning_effort: Option<String>,
    /// The configured `[multimodal]` policy.
    ///
    /// This provider normalizes image markers on its own boundary, and that
    /// normalization decodes pixels and applies `max_images` /
    /// `max_image_size_mb`. Holding the configured policy keeps the boundary
    /// pass on the same rules the runtime already applied instead of silently
    /// reverting to defaults and re-trimming an accepted history.
    multimodal: zeroclaw_config::schema::MultimodalConfig,
}

#[derive(Debug, Serialize)]
struct ResponsesRequest {
    model: String,
    input: Vec<Value>,
    instructions: String,
    store: bool,
    stream: bool,
    text: ResponsesTextOptions,
    reasoning: ResponsesReasoningOptions,
    include: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ResponsesToolSpec>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ResponsesToolSpec {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) description: String,
    /// `Arc`-shared with the tool registry's stored schema — serialized
    /// transparently, never deep-cloned per request
    pub(crate) parameters: std::sync::Arc<Value>,
    pub(crate) strict: bool,
}

#[derive(Debug, Serialize)]
struct ResponsesTextOptions {
    verbosity: String,
}

#[derive(Debug, Serialize)]
struct ResponsesReasoningOptions {
    effort: String,
    summary: String,
}

#[derive(Debug, Deserialize)]
struct ResponsesResponse {
    #[serde(default)]
    model: Option<String>,

    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    incomplete_details: Option<Value>,
    #[serde(default)]
    output: Vec<Value>,
    #[serde(default)]
    output_text: Option<String>,
    #[serde(default)]
    usage: Option<Value>,
}

#[derive(Debug, Default)]
pub(crate) struct ResponsesStreamState {
    // This decoding cursor records which protocol text components have
    // already crossed the output boundary; it never retains their bytes.
    emitted_text_parts: HashMap<(String, usize), Vec<std::ops::Range<usize>>>,
    pub(crate) saw_completion: bool,
    pub(crate) text_accumulator: String,
    pub(crate) fallback_text: Option<String>,
    pub(crate) tool_calls: HashMap<String, PendingToolCall>,
    pub(crate) emitted_tool_call_ids: HashSet<String>,
    pub(crate) collected_tool_calls: Vec<ProviderToolCall>,
    output_items: Vec<ResponsesOutputItem>,
    pub(crate) usage: Option<TokenUsage>,
}

impl ResponsesStreamState {
    pub(crate) fn saw_text_delta(&self) -> bool {
        !self.emitted_text_parts.is_empty()
    }
}

#[derive(Debug)]
struct ResponsesOutputItem {
    output_index: Option<u64>,
    item: Value,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct PendingToolCall {
    pub(crate) item_id: Option<String>,
    pub(crate) call_id: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) arguments: String,
}

#[derive(Debug, Default)]
pub(crate) struct ResponsesTurnResult {
    pub(crate) output_blocks: Vec<zeroclaw_api::model_provider::ProviderOutputBlock>,
    pub(crate) text: Option<String>,
    pub(crate) tool_calls: Vec<ProviderToolCall>,
    pub(crate) reasoning_content: Option<String>,
    pub(crate) usage: Option<TokenUsage>,
}

impl OpenAiCodexModelProvider {
    pub fn new(
        alias: &str,
        options: &ModelProviderRuntimeOptions,
        gateway_api_key: Option<&str>,
    ) -> anyhow::Result<Self> {
        let state_dir = options
            .zeroclaw_dir
            .clone()
            .unwrap_or_else(default_zeroclaw_dir);
        let auth = AuthService::new(&state_dir, options.secrets_encrypt);
        let responses_url = resolve_responses_url(options)?;

        Ok(Self {
            alias: alias.to_string(),
            auth,
            auth_profile_override: options.auth_profile_override.clone(),
            custom_endpoint: !is_default_responses_url(&responses_url),
            responses_url,
            gateway_api_key: gateway_api_key.map(ToString::to_string),
            reasoning_effort: options.reasoning_effort.clone(),
            multimodal: options.multimodal.clone(),
        })
    }

    fn http_client(&self) -> Client {
        zeroclaw_config::schema::build_runtime_proxy_client_with_read_timeout(
            "model_provider.openai",
            300,
            10,
        )
    }
}

fn default_zeroclaw_dir() -> PathBuf {
    directories::UserDirs::new().map_or_else(
        || PathBuf::from(".zeroclaw"),
        |dirs| dirs.home_dir().join(".zeroclaw"),
    )
}

fn build_responses_url(base_or_endpoint: &str) -> anyhow::Result<String> {
    let candidate = base_or_endpoint.trim();
    if candidate.is_empty() {
        anyhow::bail!("OpenAI Codex endpoint override cannot be empty");
    }

    let mut parsed = reqwest::Url::parse(candidate).map_err(|_| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"candidate": candidate})),
            "openai_codex: endpoint override is not a valid URL"
        );
        anyhow::Error::msg("OpenAI Codex endpoint override must be a valid URL")
    })?;

    match parsed.scheme() {
        "http" | "https" => {}
        _ => anyhow::bail!("OpenAI Codex endpoint override must use http:// or https://"),
    }

    let path = parsed.path().trim_end_matches('/');
    if !path.ends_with("/responses") {
        let with_suffix = if path.is_empty() || path == "/" {
            "/responses".to_string()
        } else {
            format!("{path}/responses")
        };
        parsed.set_path(&with_suffix);
    }

    parsed.set_query(None);
    parsed.set_fragment(None);

    Ok(parsed.to_string())
}

fn resolve_responses_url(options: &ModelProviderRuntimeOptions) -> anyhow::Result<String> {
    if let Some(api_url) = options
        .provider_api_url
        .as_deref()
        .and_then(|value| first_nonempty(Some(value)))
    {
        return build_responses_url(&api_url);
    }

    Ok(DEFAULT_CODEX_RESPONSES_URL.to_string())
}

fn canonical_endpoint(url: &str) -> Option<(String, String, u16, String)> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let port = parsed.port_or_known_default()?;
    let path = parsed.path().trim_end_matches('/').to_string();
    Some((parsed.scheme().to_ascii_lowercase(), host, port, path))
}

fn is_default_responses_url(url: &str) -> bool {
    canonical_endpoint(url) == canonical_endpoint(DEFAULT_CODEX_RESPONSES_URL)
}

pub(crate) fn first_nonempty(text: Option<&str>) -> Option<String> {
    text.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn normalize_model_id(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

pub(crate) fn has_turn_tools(tools: Option<&Vec<ResponsesToolSpec>>) -> bool {
    tools.as_ref().is_some_and(|t| !t.is_empty())
}

pub(crate) fn convert_tools(tools: Option<&[ToolSpec]>) -> Option<Vec<ResponsesToolSpec>> {
    let items = tools?;
    if items.is_empty() {
        return None;
    }

    Some(
        items
            .iter()
            .map(|tool| ResponsesToolSpec {
                kind: "function".to_string(),
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: std::sync::Arc::clone(&tool.parameters),
                strict: false,
            })
            .collect(),
    )
}

fn response_message_item(role: &str, content: Vec<Value>) -> Value {
    serde_json::json!({
        "type": "message",
        "role": role,
        "content": content,
    })
}

fn legacy_tool_output_message(content: &str) -> Value {
    response_message_item(
        "user",
        vec![serde_json::json!({
            "type": "input_text",
            "text": format!("Legacy tool output without call_id:\n{content}"),
        })],
    )
}

fn response_item_type(item: &Value) -> Option<&str> {
    item.get("type").and_then(Value::as_str)
}

fn is_replayable_responses_output_item(item: &Value) -> bool {
    matches!(
        response_item_type(item),
        Some("message" | "reasoning" | "function_call")
    )
}

fn encode_responses_history_items(output_items: &[Value], has_tool_calls: bool) -> Option<String> {
    if !has_tool_calls {
        return None;
    }

    let replay_items = output_items
        .iter()
        .filter(|item| is_replayable_responses_output_item(item))
        .cloned()
        .collect::<Vec<_>>();

    if !replay_items
        .iter()
        .any(|item| response_item_type(item) == Some("function_call"))
    {
        return None;
    }

    serde_json::to_string(&serde_json::json!({
        "provider": RESPONSES_HISTORY_PROVIDER,
        "kind": RESPONSES_HISTORY_KIND,
        "items": replay_items,
    }))
    .ok()
}

fn decode_responses_history_items(reasoning_content: &str) -> Option<Vec<Value>> {
    let value = serde_json::from_str::<Value>(reasoning_content).ok()?;
    if value.get("provider").and_then(Value::as_str) != Some(RESPONSES_HISTORY_PROVIDER)
        || value.get("kind").and_then(Value::as_str) != Some(RESPONSES_HISTORY_KIND)
    {
        return None;
    }

    let items = value
        .get("items")
        .and_then(Value::as_array)?
        .iter()
        .filter(|item| is_replayable_responses_output_item(item))
        .cloned()
        .collect::<Vec<_>>();

    (!items.is_empty()).then_some(items)
}

/// Build a single Responses-API `function_call` input item from a parsed
/// `ProviderToolCall`. The `arguments` field is routed through the shared
/// `sanitize_tool_arguments` helper so the openai_codex call site inherits
/// the same malformed-JSON → `"{}"` contract as the other four typed
/// providers. Factored out so the call-site behavior is testable without
/// running the full input builder.
pub(crate) fn build_function_call_item(call: ProviderToolCall) -> Value {
    let name = call.name;
    serde_json::json!({
        "type": "function_call",
        "call_id": call.id,
        "name": name,
        "arguments": crate::compatible::sanitize_tool_arguments(&name, &call.arguments),
    })
}

pub(crate) fn build_responses_input(messages: &[ChatMessage]) -> (String, Vec<Value>) {
    let mut system_parts: Vec<&str> = Vec::new();
    let mut input: Vec<Value> = Vec::new();

    for msg in messages {
        match msg.role.as_str() {
            "system" => system_parts.push(&msg.content),
            "user" => {
                let (cleaned_text, image_refs) =
                    multimodal::parse_user_message_image_refs(&msg.content);

                let mut content_items = Vec::new();

                if !cleaned_text.trim().is_empty() {
                    content_items.push(serde_json::json!({
                        "type": "input_text",
                        "text": cleaned_text,
                    }));
                }

                for image_ref in image_refs {
                    content_items.push(serde_json::json!({
                        "type": "input_image",
                        "image_url": image_ref,
                    }));
                }

                if content_items.is_empty() {
                    content_items.push(serde_json::json!({
                        "type": "input_text",
                        "text": "",
                    }));
                }

                input.push(response_message_item("user", content_items));
            }
            "assistant" => {
                if let Ok(value) = serde_json::from_str::<Value>(&msg.content)
                    && let Some(tool_calls_value) = value.get("tool_calls")
                    && let Ok(parsed_calls) =
                        serde_json::from_value::<Vec<ProviderToolCall>>(tool_calls_value.clone())
                {
                    let content = value
                        .get("content")
                        .and_then(Value::as_str)
                        .filter(|content| !content.trim().is_empty());
                    let responses_history_items = value
                        .get("reasoning_content")
                        .and_then(Value::as_str)
                        .and_then(decode_responses_history_items);

                    if let Some(items) = responses_history_items {
                        if let Some(content) = content
                            && !items
                                .iter()
                                .any(|item| response_item_type(item) == Some("message"))
                        {
                            input.push(response_message_item(
                                "assistant",
                                vec![serde_json::json!({
                                    "type": "output_text",
                                    "text": content,
                                })],
                            ));
                        }

                        input.extend(items);
                        continue;
                    }

                    if let Some(content) = value
                        .get("content")
                        .and_then(Value::as_str)
                        .filter(|content| !content.trim().is_empty())
                    {
                        input.push(response_message_item(
                            "assistant",
                            vec![serde_json::json!({
                                "type": "output_text",
                                "text": content,
                            })],
                        ));
                    }

                    for call in parsed_calls {
                        input.push(build_function_call_item(call));
                    }
                } else if !msg.content.trim().is_empty() {
                    input.push(response_message_item(
                        "assistant",
                        vec![serde_json::json!({
                            "type": "output_text",
                            "text": msg.content,
                        })],
                    ));
                }
            }
            "tool" => {
                if let Ok(value) = serde_json::from_str::<Value>(&msg.content) {
                    if let Some(call_id) = value
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .and_then(|id| first_nonempty(Some(id)))
                    {
                        let output = value
                            .get("content")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        input.push(serde_json::json!({
                            "type": "function_call_output",
                            "call_id": call_id,
                            "output": output,
                        }));
                    } else if !msg.content.trim().is_empty() {
                        input.push(legacy_tool_output_message(&msg.content));
                    }
                } else if !msg.content.trim().is_empty() {
                    input.push(legacy_tool_output_message(&msg.content));
                }
            }
            _ => {}
        }
    }

    let instructions = if system_parts.is_empty() {
        DEFAULT_CODEX_INSTRUCTIONS.to_string()
    } else {
        system_parts.join("\n\n")
    };

    (instructions, input)
}

fn clamp_reasoning_effort(model: &str, effort: &str) -> String {
    let id = normalize_model_id(model);
    // gpt-5-codex currently supports only low|medium|high.
    if id == "gpt-5-codex" {
        return match effort {
            "low" | "medium" | "high" => effort.to_string(),
            "minimal" => "low".to_string(),
            _ => "high".to_string(),
        };
    }
    if (id.starts_with("gpt-5.2") || id.starts_with("gpt-5.3")) && effort == "minimal" {
        return "low".to_string();
    }
    if id.starts_with("gpt-5-codex") && effort == "xhigh" {
        return "high".to_string();
    }
    if id == "gpt-5.1" && effort == "xhigh" {
        return "high".to_string();
    }
    if id == "gpt-5.1-codex-mini" {
        return if effort == "high" || effort == "xhigh" {
            "high".to_string()
        } else {
            "medium".to_string()
        };
    }
    effort.to_string()
}

fn resolve_reasoning_effort(model_id: &str, configured: Option<&str>) -> String {
    let raw = configured
        .and_then(|value| first_nonempty(Some(value)))
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| "xhigh".to_string());
    clamp_reasoning_effort(model_id, &raw)
}

pub(crate) fn nonempty_preserve(text: Option<&str>) -> Option<String> {
    text.and_then(|value| {
        if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        }
    })
}

fn extract_responses_text(response: &ResponsesResponse) -> Option<String> {
    if let Some(text) = nonempty_preserve(response.output_text.as_deref()) {
        return Some(text);
    }
    let text = response
        .output
        .iter()
        .filter(|item| response_item_type(item) == Some("message"))
        .flat_map(|item| {
            item.get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter(|part| matches!(response_item_type(part), Some("text" | "output_text")))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<String>();
    if !text.is_empty() {
        return Some(text);
    }

    for item in &response.output {
        if let Some(parts) = item.get("content").and_then(Value::as_array) {
            for content in parts {
                if let Some(text) = first_nonempty(content.get("text").and_then(Value::as_str)) {
                    return Some(text);
                }
            }
        }
    }

    None
}

fn extract_responses_tool_calls(response: &ResponsesResponse) -> Vec<ProviderToolCall> {
    response
        .output
        .iter()
        .filter_map(responses_tool_call)
        .collect()
}

fn responses_tool_call(item: &Value) -> Option<ProviderToolCall> {
    if response_item_type(item) != Some("function_call") {
        return None;
    }
    Some(ProviderToolCall {
        id: item
            .get("call_id")
            .and_then(Value::as_str)
            .or_else(|| item.get("id").and_then(Value::as_str))
            .map(ToString::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        name: item.get("name").and_then(Value::as_str)?.to_string(),
        arguments: item
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        extra_content: None,
    })
}

fn responses_turn_from_response(response: &ResponsesResponse) -> ResponsesTurnResult {
    let tool_calls = extract_responses_tool_calls(response);
    let reasoning_content =
        encode_responses_history_items(&response.output, !tool_calls.is_empty());

    let mut output_blocks = crate::output::responses_blocks(&response.output);
    if let Some(model) = &response.model {
        output_blocks.push(zeroclaw_api::model_provider::ProviderOutputBlock::ProtocolMetadata {metadata:serde_json::json!({"kind":"served_attribution","protocol":"responses","model":model})});
    }
    output_blocks.push(
        zeroclaw_api::model_provider::ProviderOutputBlock::ProtocolMetadata {
            metadata: serde_json::json!({
                "kind":"finish", "protocol":"responses", "status":response.status,
                "incomplete_details":response.incomplete_details,
            }),
        },
    );
    ResponsesTurnResult {
        output_blocks,
        text: extract_responses_text(response),
        tool_calls,
        reasoning_content,
        usage: parse_responses_usage(response.usage.as_ref()),
    }
}

/// Parses usage from a Responses response without rejecting an otherwise valid
/// response when optional usage fields are absent or malformed.
pub(crate) fn parse_responses_usage(usage: Option<&Value>) -> Option<TokenUsage> {
    let usage = usage?.as_object()?;
    let input_tokens = usage.get("input_tokens").and_then(Value::as_u64);
    let output_tokens = usage.get("output_tokens").and_then(Value::as_u64);
    let cached_input_tokens = usage
        .get("input_tokens_details")
        .and_then(Value::as_object)
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64);

    (input_tokens.is_some() || output_tokens.is_some() || cached_input_tokens.is_some()).then_some(
        TokenUsage {
            input_tokens,
            output_tokens,
            cached_input_tokens,
            cache_creation_input_tokens: None,
        },
    )
}

fn record_responses_output_item(
    state: &mut ResponsesStreamState,
    output_index: Option<u64>,
    item: Value,
) -> Option<Value> {
    let item_id = item
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| item.get("call_id").and_then(Value::as_str));
    // A protocol coordinate identifies the item even if a compatible server
    // changes or omits its ID. Bind unidentified done items only once when
    // their terminal coordinate becomes available.
    if let Some(existing) = state.output_items.iter_mut().find(|existing| {
        if let (Some(left), Some(right)) = (existing.output_index, output_index) {
            return left == right;
        }
        let existing_id = existing
            .item
            .get("id")
            .and_then(Value::as_str)
            .or_else(|| existing.item.get("call_id").and_then(Value::as_str));
        match (existing_id, item_id) {
            (Some(left), Some(right)) => left == right,
            _ => existing.output_index.is_none() && output_index.is_some() && existing.item == item,
        }
    }) {
        if existing.output_index.is_none() {
            existing.output_index = output_index;
        }
        return Some(std::mem::replace(&mut existing.item, item));
    }
    state
        .output_items
        .push(ResponsesOutputItem { output_index, item });
    None
}

fn replace_responses_output_items(state: &mut ResponsesStreamState, items: &[Value]) {
    if !items.is_empty() {
        state.output_items = items
            .iter()
            .enumerate()
            .map(|(index, item)| ResponsesOutputItem {
                output_index: u64::try_from(index).ok(),
                item: item.clone(),
            })
            .collect();
    }
}

fn text_part_key(
    item_id: Option<&str>,
    output_index: Option<u64>,
    content_index: usize,
) -> (String, usize) {
    (
        output_index
            .map(|index| format!("output:{index}"))
            .or_else(|| item_id.map(|id| format!("item:{id}")))
            .unwrap_or_else(|| "unattributed".into()),
        content_index,
    )
}

fn emit_content_position(
    output_index: Option<u64>,
    content_index: Option<u64>,
    emitted: &mut Vec<StreamEvent>,
) {
    if let Some(output_index) = output_index {
        emitted.push(StreamEvent::ContentPosition(
            zeroclaw_api::model_provider::ProviderContentPosition {
                output_index,
                content_index,
            },
        ));
    }
}

fn bind_responses_text_part(
    item_id: Option<&str>,
    output_index: Option<u64>,
    content_index: usize,
    state: &mut ResponsesStreamState,
) -> (String, usize) {
    let key = text_part_key(item_id, output_index, content_index);
    if !state.emitted_text_parts.contains_key(&key) {
        let prior = item_id
            .and_then(|id| {
                state
                    .emitted_text_parts
                    .remove(&(format!("item:{id}"), content_index))
            })
            .or_else(|| {
                // Older compatible streams omitted the component identity.
                state
                    .emitted_text_parts
                    .remove(&("unattributed".into(), content_index))
            });
        if let Some(ranges) = prior {
            state.emitted_text_parts.insert(key.clone(), ranges);
        }
    }
    key
}

fn emit_responses_text_delta(
    text: &str,
    key: (String, usize),
    output_index: Option<u64>,
    state: &mut ResponsesStreamState,
    emitted: &mut Vec<StreamEvent>,
) {
    let content_index = key.1;
    let start = state.text_accumulator.len();
    state.text_accumulator.push_str(text);
    let end = state.text_accumulator.len();
    let ranges = state.emitted_text_parts.entry(key).or_default();
    if let Some(previous) = ranges.last_mut()
        && previous.end == start
    {
        previous.end = end;
    } else {
        ranges.push(start..end);
    }
    emit_content_position(output_index, u64::try_from(content_index).ok(), emitted);
    emitted.push(StreamEvent::TextDelta(StreamChunk::delta(text)));
}

fn emit_responses_text_part(
    text: Option<&str>,
    item_id: Option<&str>,
    output_index: Option<u64>,
    content_index: usize,
    evidence: &Value,
    state: &mut ResponsesStreamState,
    emitted: &mut Vec<StreamEvent>,
) -> anyhow::Result<()> {
    let Some(text) = text else {
        return Ok(());
    };
    let key = bind_responses_text_part(item_id, output_index, content_index, state);
    let mut offset = 0;
    // The cursor only indexes ranges in the existing canonical accumulator;
    // a done frame can supply a suffix without retaining a second text copy.
    for range in state.emitted_text_parts.get(&key).into_iter().flatten() {
        let observed = &state.text_accumulator[range.clone()];
        if !text
            .get(offset..)
            .is_some_and(|remaining| remaining.starts_with(observed))
        {
            let detail = serde_json::json!({
                "item_id":item_id,
                "output_index":output_index,
                "content_index":content_index,
                "completion":evidence,
            });
            return Err(StreamError::InvalidSse(format!(
                "Responses text completion contradicts previously observed text ({}): {detail}",
                evidence
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("message")
            ))
            .into());
        }
        offset += observed.len();
    }
    if let Some(suffix) = text.get(offset..).filter(|suffix| !suffix.is_empty()) {
        emit_responses_text_delta(suffix, key, output_index, state, emitted);
    }
    Ok(())
}

fn emit_responses_message_parts(
    item: &Value,
    output_index: Option<u64>,
    previous_item: Option<&Value>,
    state: &mut ResponsesStreamState,
    emitted: &mut Vec<StreamEvent>,
) -> anyhow::Result<()> {
    let Some(parts) = item.get("content").and_then(Value::as_array) else {
        return Ok(());
    };
    if crate::output::message_metadata(item)
        != previous_item.and_then(crate::output::message_metadata)
        && let Some(metadata) = crate::output::message_metadata(item)
    {
        emit_content_position(output_index, None, emitted);
        emitted.push(StreamEvent::ProtocolMetadata(metadata));
    }
    for (content_index, part) in parts.iter().enumerate() {
        if matches!(
            part.get("type").and_then(Value::as_str),
            Some("text" | "output_text")
        ) {
            emit_responses_text_part(
                part.get("text").and_then(Value::as_str),
                item.get("id").and_then(Value::as_str),
                output_index,
                content_index,
                item,
                state,
                emitted,
            )?;
        } else if previous_item
            .and_then(|item| item.get("content"))
            .and_then(Value::as_array)
            .and_then(|parts| parts.get(content_index))
            != Some(part)
        {
            emit_content_position(output_index, u64::try_from(content_index).ok(), emitted);
            emitted.push(StreamEvent::OutputBlock(crate::output::content_block(part)));
        }
        if previous_item
            .and_then(|item| item.get("content"))
            .and_then(Value::as_array)
            .and_then(|parts| parts.get(content_index))
            != Some(part)
            && let Some(metadata) = crate::output::content_metadata(part)
        {
            emit_content_position(output_index, u64::try_from(content_index).ok(), emitted);
            emitted.push(StreamEvent::OutputBlock(metadata));
        }
    }
    Ok(())
}

fn pending_tool_call_key(item_id: Option<&str>, output_index: Option<u64>) -> Option<String> {
    item_id
        .map(ToString::to_string)
        .or_else(|| output_index.map(|index| format!("output:{index}")))
}

fn emit_tool_call(
    state: &mut ResponsesStreamState,
    tool_call: ProviderToolCall,
) -> Option<ProviderToolCall> {
    if state.emitted_tool_call_ids.insert(tool_call.id.clone()) {
        state.collected_tool_calls.push(tool_call.clone());
        Some(tool_call)
    } else {
        None
    }
}

pub(crate) type ResponsesStreamApiError = crate::dispatch::ProviderProtocolFailure;

pub(crate) fn process_responses_stream_event(
    event: Value,
    state: &mut ResponsesStreamState,
) -> anyhow::Result<Vec<StreamEvent>> {
    if matches!(
        event.get("type").and_then(Value::as_str),
        Some("error" | "response.failed")
    ) {
        return Err(ResponsesStreamApiError::new(event).into());
    }

    let mut emitted = Vec::new();
    if let Some(model) = event
        .get("response")
        .and_then(|r| r.get("model"))
        .and_then(Value::as_str)
    {
        emitted.push(StreamEvent::ProtocolMetadata(
            serde_json::json!({"kind":"served_attribution","protocol":"responses","model":model}),
        ));
    }
    if matches!(
        event.get("type").and_then(Value::as_str),
        Some("response.output_text.delta" | "response.output_text.done")
    ) {
        let fields: serde_json::Map<String, Value> = ["annotations", "logprobs"]
            .into_iter()
            .filter_map(|key| event.get(key).map(|value| (key.into(), value.clone())))
            .collect();
        if !fields.is_empty() {
            emit_content_position(
                event.get("output_index").and_then(Value::as_u64),
                event.get("content_index").and_then(Value::as_u64),
                &mut emitted,
            );
            emitted.push(StreamEvent::ProtocolMetadata(serde_json::json!({"kind":"content_metadata","type_name":"output_text","fields":fields})));
        }
    }
    match event.get("type").and_then(Value::as_str) {
        Some("response.output_text.delta") => {
            if let Some(text) = nonempty_preserve(event.get("delta").and_then(Value::as_str)) {
                let output_index = event.get("output_index").and_then(Value::as_u64);
                let key = bind_responses_text_part(
                    event.get("item_id").and_then(Value::as_str),
                    output_index,
                    event
                        .get("content_index")
                        .and_then(Value::as_u64)
                        .and_then(|index| usize::try_from(index).ok())
                        .unwrap_or(0),
                    state,
                );
                emit_responses_text_delta(&text, key, output_index, state, &mut emitted);
            }
        }
        Some("response.reasoning_summary_text.delta" | "response.reasoning_text.delta") => {
            if let Some(text) = nonempty_preserve(event.get("delta").and_then(Value::as_str)) {
                emit_content_position(
                    event.get("output_index").and_then(Value::as_u64),
                    None,
                    &mut emitted,
                );
                emitted.push(StreamEvent::ThinkingDelta(text));
            }
        }
        Some("response.output_text.done") => {
            emit_responses_text_part(
                event.get("text").and_then(Value::as_str),
                event.get("item_id").and_then(Value::as_str),
                event.get("output_index").and_then(Value::as_u64),
                event
                    .get("content_index")
                    .and_then(Value::as_u64)
                    .and_then(|index| usize::try_from(index).ok())
                    .unwrap_or(0),
                &event,
                state,
                &mut emitted,
            )?;
        }
        Some("response.output_item.added") => {
            let item = event.get("item");
            let item_type = item
                .and_then(|value| value.get("type"))
                .and_then(Value::as_str);
            if let Some(item) = item {
                let metadata = if item_type == Some("message") {
                    crate::output::message_metadata(item).map(|metadata| {
                        zeroclaw_api::model_provider::ProviderOutputBlock::ProtocolMetadata {
                            metadata,
                        }
                    })
                } else {
                    crate::output::content_metadata(item)
                };
                if let Some(metadata) = metadata {
                    emit_content_position(
                        event.get("output_index").and_then(Value::as_u64),
                        None,
                        &mut emitted,
                    );
                    emitted.push(StreamEvent::OutputBlock(metadata));
                }
            }
            if item_type == Some("function_call") {
                let key = pending_tool_call_key(
                    item.and_then(|value| value.get("id"))
                        .and_then(Value::as_str),
                    event.get("output_index").and_then(Value::as_u64),
                );
                if let Some(key) = key {
                    let entry = state.tool_calls.entry(key).or_default();
                    entry.item_id = item
                        .and_then(|value| value.get("id"))
                        .and_then(Value::as_str)
                        .map(ToString::to_string);
                    entry.call_id = item
                        .and_then(|value| value.get("call_id"))
                        .and_then(Value::as_str)
                        .map(ToString::to_string);
                    entry.name = item
                        .and_then(|value| value.get("name"))
                        .and_then(Value::as_str)
                        .map(ToString::to_string);
                    if let Some(arguments) = item
                        .and_then(|value| value.get("arguments"))
                        .and_then(Value::as_str)
                    {
                        entry.arguments = arguments.to_string();
                    }
                }
            }
        }
        Some("response.output_text.annotation.added") => {
            if let Some(annotation) = event.get("annotation") {
                emit_content_position(
                    event.get("output_index").and_then(Value::as_u64),
                    event.get("content_index").and_then(Value::as_u64),
                    &mut emitted,
                );
                emitted.push(StreamEvent::ProtocolMetadata(serde_json::json!({
                    "kind":"content_metadata", "type_name":"output_text",
                    "fields":{"annotations":[annotation]}, "annotation_index":event.get("annotation_index"),
                })));
            }
        }
        Some("response.function_call_arguments.delta") => {
            if let Some(key) = pending_tool_call_key(
                event.get("item_id").and_then(Value::as_str),
                event.get("output_index").and_then(Value::as_u64),
            ) {
                let entry = state.tool_calls.entry(key).or_default();
                entry.item_id = event
                    .get("item_id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                entry.arguments.push_str(
                    event
                        .get("delta")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                );
                emitted.push(StreamEvent::ProtocolMetadata(serde_json::json!({
                    "kind":"tool_arguments_delta", "protocol":"responses",
                    "item_id":event.get("item_id"), "output_index":event.get("output_index"),
                    "delta":event.get("delta"),
                })));
            }
        }
        Some("response.function_call_arguments.done") => {
            let key = pending_tool_call_key(
                event.get("item_id").and_then(Value::as_str),
                event.get("output_index").and_then(Value::as_u64),
            );
            let mut pending = key
                .as_ref()
                .and_then(|key| state.tool_calls.remove(key))
                .unwrap_or_default();
            pending.item_id = pending.item_id.or_else(|| {
                event
                    .get("item_id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            });
            pending.call_id = pending.call_id.or_else(|| {
                event
                    .get("call_id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            });
            pending.name = pending.name.or_else(|| {
                event
                    .get("name")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            });
            if let Some(arguments) = event.get("arguments").and_then(Value::as_str) {
                pending.arguments = arguments.to_string();
            }

            if let Some(name) = pending.name {
                let tool_call = ProviderToolCall {
                    id: pending
                        .call_id
                        .or(pending.item_id)
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    name,
                    arguments: pending.arguments,
                    extra_content: None,
                };
                if let Some(tool_call) = emit_tool_call(state, tool_call) {
                    emit_content_position(
                        event.get("output_index").and_then(Value::as_u64),
                        None,
                        &mut emitted,
                    );
                    emitted.push(StreamEvent::ToolCall(tool_call));
                }
            }
        }
        Some("response.output_item.done") => {
            if let Some(item) = event.get("item") {
                let previous_item = record_responses_output_item(
                    state,
                    event.get("output_index").and_then(Value::as_u64),
                    item.clone(),
                );
                let first_observation = previous_item.is_none();
                if item.get("type").and_then(Value::as_str) == Some("function_call")
                    && let Some(metadata) = crate::output::content_metadata(item)
                {
                    emit_content_position(
                        event.get("output_index").and_then(Value::as_u64),
                        None,
                        &mut emitted,
                    );
                    emitted.push(StreamEvent::OutputBlock(metadata));
                }
                if item.get("type").and_then(Value::as_str) == Some("message") {
                    emit_responses_message_parts(
                        item,
                        event.get("output_index").and_then(Value::as_u64),
                        previous_item.as_ref(),
                        state,
                        &mut emitted,
                    )?;
                } else if first_observation
                    && item.get("type").and_then(Value::as_str) != Some("function_call")
                {
                    for block in crate::output::content_blocks(item) {
                        emit_content_position(
                            event.get("output_index").and_then(Value::as_u64),
                            None,
                            &mut emitted,
                        );
                        emitted.push(StreamEvent::OutputBlock(block));
                    }
                }
                if first_observation
                    && let Some(tool_call) = responses_tool_call(item)
                    && let Some(tool_call) = emit_tool_call(state, tool_call)
                {
                    emit_content_position(
                        event.get("output_index").and_then(Value::as_u64),
                        None,
                        &mut emitted,
                    );
                    emitted.push(StreamEvent::ToolCall(tool_call));
                }
            }
        }
        Some(event_type @ ("response.completed" | "response.done")) => {
            state.saw_completion = true;
            emitted.push(StreamEvent::ProtocolMetadata(serde_json::json!({
                "kind":"finish", "protocol":"responses", "event_type":event_type,
                "status":event.get("response").and_then(|r|r.get("status")),
                "incomplete_details":event.get("response").and_then(|r|r.get("incomplete_details")),
            })));
            let is_completed = event_type == "response.completed";
            if let Some(response) = event
                .get("response")
                .and_then(|value| serde_json::from_value::<ResponsesResponse>(value.clone()).ok())
            {
                if !state.saw_text_delta() && state.fallback_text.is_none() {
                    state.fallback_text = extract_responses_text(&response);
                }
                // Only done items have entered output_items. A terminal-only
                // response must deliver semantic blocks that had no item.done
                // frame; reuse that canonical parser state for deduplication.
                for (output_index, item) in response.output.iter().enumerate() {
                    let previous_item = record_responses_output_item(
                        state,
                        u64::try_from(output_index).ok(),
                        item.clone(),
                    );
                    let first_observation = previous_item.is_none();
                    if item.get("type").and_then(Value::as_str) == Some("message") {
                        emit_responses_message_parts(
                            item,
                            u64::try_from(output_index).ok(),
                            previous_item.as_ref(),
                            state,
                            &mut emitted,
                        )?;
                        continue;
                    }
                    if first_observation {
                        if let Some(tool_call) = responses_tool_call(item) {
                            if let Some(metadata) = crate::output::content_metadata(item) {
                                emit_content_position(
                                    u64::try_from(output_index).ok(),
                                    None,
                                    &mut emitted,
                                );
                                emitted.push(StreamEvent::OutputBlock(metadata));
                            }
                            if let Some(tool_call) = emit_tool_call(state, tool_call) {
                                emit_content_position(
                                    u64::try_from(output_index).ok(),
                                    None,
                                    &mut emitted,
                                );
                                emitted.push(StreamEvent::ToolCall(tool_call));
                            }
                            continue;
                        }
                        for block in crate::output::responses_blocks(std::slice::from_ref(item)) {
                            if !matches!(
                                block,
                                zeroclaw_api::model_provider::ProviderOutputBlock::Text { .. }
                                    | zeroclaw_api::model_provider::ProviderOutputBlock::ToolCall { .. }
                            ) {
                                emit_content_position(
                                    u64::try_from(output_index).ok(),
                                    None,
                                    &mut emitted,
                                );
                                emitted.push(StreamEvent::OutputBlock(block));
                            }
                        }
                    }
                }
                replace_responses_output_items(state, &response.output);
                if is_completed && let Some(usage) = parse_responses_usage(response.usage.as_ref())
                {
                    state.usage = Some(usage.clone());
                    emitted.push(StreamEvent::Usage(usage));
                }
            }
        }
        Some(kind)
            if kind.starts_with("response.")
                && (kind.contains("_call.") || kind.contains("_call_")) =>
        {
            emit_content_position(
                event.get("output_index").and_then(Value::as_u64),
                event.get("content_index").and_then(Value::as_u64),
                &mut emitted,
            );
            emitted.push(StreamEvent::ProtocolMetadata(serde_json::json!({
                "kind":"provider_tool_activity", "protocol":"responses", "activity":event,
            })));
        }
        Some(kind)
            if kind.starts_with("response.output_")
                || kind.starts_with("response.content_part.")
                || kind.starts_with("response.reasoning_") =>
        {
            emit_content_position(
                event.get("output_index").and_then(Value::as_u64),
                event.get("content_index").and_then(Value::as_u64),
                &mut emitted,
            );
            emitted.push(StreamEvent::ProtocolMetadata(serde_json::json!({
                "kind":"partial_content", "protocol":"responses", "type_name":kind,
                "component": if kind.contains("reasoning") {"reasoning"} else {"opaque"},
                "activity":event,
            })));
        }
        _ => {}
    }

    Ok(emitted)
}

pub(crate) fn process_sse_chunk(
    chunk: &str,
    state: &mut ResponsesStreamState,
) -> anyhow::Result<Vec<StreamEvent>> {
    let data_lines: Vec<String> = chunk
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|line| line.trim().to_string())
        .collect();
    if data_lines.is_empty() {
        return Ok(Vec::new());
    }

    let joined = data_lines.join("\n");
    let trimmed = joined.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" {
        if trimmed == "[DONE]" {
            state.saw_completion = true;
        }
        return Ok(Vec::new());
    }

    if let Ok(event) = serde_json::from_str::<Value>(trimmed) {
        return process_responses_stream_event(event, state);
    }

    let mut emitted = Vec::new();
    for line in data_lines {
        let line = line.trim();
        if line.is_empty() || line == "[DONE]" {
            if line == "[DONE]" {
                state.saw_completion = true;
            }
            continue;
        }
        let event = serde_json::from_str::<Value>(line).map_err(|err| {
            let sanitized = super::sanitize_api_error(line);
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "phase": "sse_parse",
                        "payload": &sanitized,
                        "error": format!("{}", err),
                    })),
                "openai_codex: SSE data parse failed"
            );
            anyhow::Error::new(err).context(format!(
                "OpenAI Codex SSE data parse failed. Payload: {sanitized}"
            ))
        })?;
        emitted.extend(process_responses_stream_event(event, state)?);
    }

    Ok(emitted)
}

pub(crate) fn incomplete_responses_sse_error(buffer: &str) -> Option<StreamError> {
    if buffer.trim().is_empty() {
        return None;
    }
    let detail = super::scrub_secret_patterns(buffer);
    Some(StreamError::InvalidSse(format!(
        "Responses stream ended before an SSE frame delimiter: {detail}"
    )))
}

fn parse_sse_turn(body: &str) -> anyhow::Result<ResponsesTurnResult> {
    let mut state = ResponsesStreamState::default();
    let mut buffer = body.to_string();

    while let Some(idx) = buffer.find("\n\n") {
        let chunk = buffer[..idx].to_string();
        buffer = buffer[idx + 2..].to_string();
        process_sse_chunk(&chunk, &mut state)?;
    }

    if !buffer.trim().is_empty() {
        process_sse_chunk(&buffer, &mut state)?;
    }

    Ok(responses_turn_from_state(state))
}

fn responses_turn_from_state(state: ResponsesStreamState) -> ResponsesTurnResult {
    let saw_text_delta = state.saw_text_delta();
    let mut output_items = state.output_items;
    output_items.sort_by(
        |left, right| match (left.output_index, right.output_index) {
            (Some(left), Some(right)) => left.cmp(&right),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        },
    );
    let output_items: Vec<_> = output_items.into_iter().map(|entry| entry.item).collect();
    ResponsesTurnResult {
        output_blocks: crate::output::responses_blocks(&output_items),
        text: if saw_text_delta {
            nonempty_preserve(Some(&state.text_accumulator))
        } else {
            state.fallback_text
        },
        reasoning_content: encode_responses_history_items(
            &output_items,
            !state.collected_tool_calls.is_empty(),
        ),
        tool_calls: state.collected_tool_calls,
        usage: state.usage,
    }
}

async fn observe_responses_sse_chunk(
    chunk: &str,
    state: &mut ResponsesStreamState,
) -> anyhow::Result<()> {
    let mut events = process_sse_chunk(chunk, state)?;
    if state.saw_completion
        && !state.saw_text_delta()
        && let Some(text) = state.fallback_text.clone()
    {
        emit_responses_text_delta(&text, ("unattributed".into(), 0), None, state, &mut events);
    }
    for event in events {
        crate::dispatch::capture::observe_adapter_event(&event).await?;
    }
    Ok(())
}

fn ensure_nonempty_responses_turn(
    result: ResponsesTurnResult,
    empty_error: impl FnOnce() -> anyhow::Error,
) -> anyhow::Result<ResponsesTurnResult> {
    if result.text.as_deref().is_some_and(|text| !text.is_empty())
        || !result.tool_calls.is_empty()
        || result.output_blocks.iter().any(|block| {
            matches!(
                block,
                zeroclaw_api::model_provider::ProviderOutputBlock::Media { .. }
            )
        })
    {
        Ok(result)
    } else {
        Err(empty_error())
    }
}

pub(crate) fn append_utf8_stream_chunk(
    body: &mut String,
    pending: &mut Vec<u8>,
    chunk: &[u8],
) -> anyhow::Result<()> {
    if pending.is_empty()
        && let Ok(text) = std::str::from_utf8(chunk)
    {
        body.push_str(text);
        return Ok(());
    }

    if !chunk.is_empty() {
        pending.extend_from_slice(chunk);
    }
    if pending.is_empty() {
        return Ok(());
    }

    match std::str::from_utf8(pending) {
        Ok(text) => {
            body.push_str(text);
            pending.clear();
            Ok(())
        }
        Err(err) => {
            let valid_up_to = err.valid_up_to();
            if valid_up_to > 0 {
                // SAFETY: `valid_up_to` always points to the end of a valid UTF-8 prefix.
                let prefix = std::str::from_utf8(&pending[..valid_up_to])
                    .expect("valid UTF-8 prefix from Utf8Error::valid_up_to");
                body.push_str(prefix);
                pending.drain(..valid_up_to);
            }

            if err.error_len().is_some() {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "phase": "utf8_decode",
                            "error": format!("{}", err),
                        })),
                    "openai_codex: response contained invalid UTF-8"
                );
                return Err(anyhow::Error::new(err)
                    .context("OpenAI Codex response contained invalid UTF-8"));
            }

            // `error_len == None` means we have a valid prefix and an incomplete
            // multi-byte sequence at the end; keep it buffered until next chunk.
            Ok(())
        }
    }
}

fn parse_responses_body(body: &str) -> anyhow::Result<ResponsesTurnResult> {
    let body_trimmed = body.trim_start();
    let looks_like_sse = body_trimmed.starts_with("event:") || body_trimmed.starts_with("data:");
    if looks_like_sse {
        let result = parse_sse_turn(body)?;
        return ensure_nonempty_responses_turn(result, || {
            let sanitized = super::sanitize_api_error(body);
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"payload": &sanitized})),
                "openai_codex: empty SSE stream payload"
            );
            anyhow::Error::msg(format!(
                "No response from OpenAI Codex stream payload: {sanitized}"
            ))
        });
    }

    let parsed: ResponsesResponse = serde_json::from_str(body).map_err(|err| {
        let sanitized = super::sanitize_api_error(body);
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "payload": &sanitized,
                    "error": format!("{}", err),
                })),
            "openai_codex: JSON parse failed"
        );
        anyhow::Error::new(err).context(format!(
            "OpenAI Codex JSON parse failed. Payload: {sanitized}"
        ))
    })?;
    let result = responses_turn_from_response(&parsed);
    ensure_nonempty_responses_turn(result, || {
        let sanitized = super::sanitize_api_error(body);
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"payload": &sanitized})),
            "openai_codex: empty response"
        );
        anyhow::Error::msg(format!("No response from OpenAI Codex: {sanitized}"))
    })
}

async fn decode_responses_body(response: reqwest::Response) -> anyhow::Result<ResponsesTurnResult> {
    let mut body = String::new();
    let mut pending_utf8 = Vec::new();
    let mut state = ResponsesStreamState::default();
    let mut is_sse = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"));
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|err| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "phase": "stream_read",
                        "error": format!("{}", err),
                    })),
                "openai_codex: error reading response stream"
            );
            anyhow::Error::new(super::stream_read_error(
                &err,
                crate::StreamIdleBound::Fixed(crate::STREAM_IDLE_TIMEOUT),
            ))
            .context("error reading OpenAI Codex response stream")
        })?;
        let decode_result = append_utf8_stream_chunk(&mut body, &mut pending_utf8, &bytes);
        if zeroclaw_api::session_events::capture::current_provider_admission().is_some() {
            anyhow::ensure!(
                body.len() <= 32 * 1024 * 1024,
                "session_capture_wire_frame_limit_exceeded"
            );
        }
        is_sse |= body.trim_start().starts_with("event:") || body.trim_start().starts_with("data:");
        if is_sse {
            body = body.replace("\r\n", "\n");
            while let Some(end) = body.find("\n\n") {
                let chunk = body[..end].to_string();
                body.drain(..end + 2);
                observe_responses_sse_chunk(&chunk, &mut state).await?;
                if state.saw_completion {
                    body.clear();
                    break;
                }
            }
        }
        decode_result?;
        if state.saw_completion {
            break;
        }
    }

    if !pending_utf8.is_empty() {
        let err = match std::str::from_utf8(&pending_utf8) {
            Err(e) => e,
            Ok(_) => {
                // Structurally unreachable: append_utf8_stream_chunk only accumulates
                // incomplete multi-byte sequences (error_len == None), so from_utf8
                // always returns Err here. Handled as an error rather than a panic so
                // the daemon survives if the invariant is somehow violated.
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "openai_codex: pending bytes were valid UTF-8 (invariant violated)"
                );
                return Err(anyhow::Error::msg(
                    "OpenAI Codex response stream ended with valid UTF-8 in pending bytes (unexpected)",
                ));
            }
        };
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
            "openai_codex: response ended with incomplete UTF-8"
        );
        return Err(
            anyhow::Error::new(err).context("OpenAI Codex response ended with incomplete UTF-8")
        );
    }

    let result = if is_sse {
        if let Some(error) = incomplete_responses_sse_error(&body) {
            return Err(error.into());
        }
        anyhow::ensure!(state.saw_completion, "stream_eof_without_final");
        ensure_nonempty_responses_turn(responses_turn_from_state(state), || {
            anyhow::Error::msg("codex_empty_sse_response")
        })?
    } else {
        parse_responses_body(&body)?
    };
    crate::dispatch::capture::observe_adapter_response(
        &ProviderChatResponse {
            output_blocks: result.output_blocks.clone(),
            text: result.text.clone(),
            tool_calls: result.tool_calls.clone(),
            usage: result.usage.clone(),
            reasoning_content: result.reasoning_content.clone(),
        },
        is_sse,
    )
    .await?;
    Ok(result)
}

struct ResolvedCodexCredentials {
    bearer_token: String,
    account_id: Option<String>,
    access_token: Option<String>,
    use_gateway_api_key_auth: bool,
}

impl OpenAiCodexModelProvider {
    async fn resolve_credentials(&self) -> anyhow::Result<ResolvedCodexCredentials> {
        let use_gateway_api_key_auth = self.custom_endpoint && self.gateway_api_key.is_some();

        let profile = match self
            .auth
            .get_profile("openai-codex", self.auth_profile_override.as_deref())
            .await
        {
            Ok(profile) => profile,
            Err(err) if use_gateway_api_key_auth => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
                    "failed to load OpenAI Codex profile; continuing with custom endpoint API key mode"
                );
                None
            }
            Err(err) => return Err(err),
        };

        let oauth_access_token = match self
            .auth
            .get_valid_openai_access_token(self.auth_profile_override.as_deref())
            .await
        {
            Ok(token) => token,
            Err(err) if use_gateway_api_key_auth => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
                    "failed to refresh OpenAI token; continuing with custom endpoint API key mode"
                );
                None
            }
            Err(err) => return Err(err),
        };

        // Distinguish "no credentials at all" from "credentials present but
        // unusable" (expired / could not be refreshed) so the call-time error
        // stops blaming a missing profile when the real fix is re-authentication.
        let had_profile = profile.is_some();

        let account_id = profile.and_then(|p| p.account_id).or_else(|| {
            oauth_access_token
                .as_deref()
                .and_then(extract_account_id_from_jwt)
        });

        let access_token = if use_gateway_api_key_auth {
            oauth_access_token
        } else {
            Some(oauth_access_token.ok_or_else(|| {
                if had_profile {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "missing": "oauth_access_token",
                                "had_profile": true,
                            })),
                        "openai_codex: auth profile present but no usable access token"
                    );
                    anyhow::Error::msg(
                        "OpenAI Codex credentials are present but expired or could not be refreshed. Re-run `zeroclaw auth login --model-provider openai-codex` to sign in again.",
                    )
                } else {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "missing": "oauth_access_token",
                                "had_profile": false,
                            })),
                        "openai_codex: no auth profile found"
                    );
                    anyhow::Error::msg(
                        "No OpenAI Codex credentials found. Run `zeroclaw auth login --model-provider openai-codex` to sign in.",
                    )
                }
            })?)
        };

        let account_id = if use_gateway_api_key_auth {
            account_id
        } else {
            Some(account_id.ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"missing": "account_id"})),
                    "openai_codex: account_id not found in profile/token"
                );
                anyhow::Error::msg(
                    "OpenAI Codex account id not found in auth profile/token. Run `zeroclaw auth login --model-provider openai-codex` again.",
                )
            })?)
        };

        let bearer_token = if use_gateway_api_key_auth {
            self.gateway_api_key.clone().unwrap_or_default()
        } else {
            access_token.clone().unwrap_or_default()
        };

        Ok(ResolvedCodexCredentials {
            bearer_token,
            account_id,
            access_token,
            use_gateway_api_key_auth,
        })
    }

    fn responses_request_builder(
        &self,
        bearer_token: &str,
        account_id: Option<&str>,
        access_token: Option<&str>,
        use_gateway_api_key_auth: bool,
        request: &ResponsesRequest,
    ) -> reqwest::RequestBuilder {
        let mut request_builder = self
            .http_client()
            .post(&self.responses_url)
            .header("Authorization", format!("Bearer {bearer_token}"))
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "pi")
            .header("Content-Type", "application/json");

        if request.stream {
            request_builder = request_builder.header("accept", "text/event-stream");
        }

        if let Some(account_id) = account_id {
            request_builder = request_builder.header("chatgpt-account-id", account_id);
        }

        if use_gateway_api_key_auth {
            if let Some(access_token) = access_token {
                request_builder = request_builder.header("x-openai-access-token", access_token);
            }
            if let Some(account_id) = account_id {
                request_builder = request_builder.header("x-openai-account-id", account_id);
            }
        }

        request_builder
    }

    async fn send_responses_request(
        &self,
        input: Vec<Value>,
        instructions: String,
        tools: Option<Vec<ResponsesToolSpec>>,
        model: &str,
    ) -> anyhow::Result<ResponsesTurnResult> {
        let creds = self.resolve_credentials().await?;
        let normalized_model = normalize_model_id(model);

        let tools_count = tools.as_ref().map_or(0, Vec::len);
        let has_tools = has_turn_tools(tools.as_ref());
        let mut request = ResponsesRequest {
            model: normalized_model.to_string(),
            input,
            instructions,
            store: false,
            stream: true,
            text: ResponsesTextOptions {
                verbosity: "medium".to_string(),
            },
            reasoning: ResponsesReasoningOptions {
                effort: resolve_reasoning_effort(
                    normalized_model,
                    self.reasoning_effort.as_deref(),
                ),
                summary: "auto".to_string(),
            },
            include: vec!["reasoning.encrypted_content".to_string()],
            tools,
            tool_choice: has_tools.then(|| "auto".to_string()),
            parallel_tool_calls: has_tools.then_some(true),
        };
        if ::zeroclaw_log::debug_enabled() {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                    .with_attrs(::serde_json::json!({
                        "provider": "openai_codex",
                        "alias": &self.alias,
                        "request_api": WIRE_API,
                        "model": &request.model,
                        "stream": true,
                        "tools_count": tools_count,
                        "tool_choice": request.tool_choice.as_deref(),
                        "parallel_tool_calls": request.parallel_tool_calls,
                    })),
                "openai codex responses provider request prepared"
            );
        }

        let request_builder = self.responses_request_builder(
            &creds.bearer_token,
            creds.account_id.as_deref(),
            creds.access_token.as_deref(),
            creds.use_gateway_api_key_auth,
            &request,
        );

        let response = request_builder.json(&request).send().await?;

        if !response.status().is_success() {
            return Err(super::api_error("OpenAI Codex", response).await);
        }

        match decode_responses_body(response).await {
            Ok(result) => Ok(result),
            Err(stream_err) => {
                // Native dispatch owns one physical attempt. An unresolved
                // response must reach its recorder before any new request;
                // never hide a second effect in this decoder fallback.
                if zeroclaw_api::session_events::capture::current_provider_admission().is_some() {
                    return Err(stream_err);
                }
                if stream_err
                    .downcast_ref::<ResponsesStreamApiError>()
                    .is_some()
                {
                    return Err(stream_err);
                }

                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", stream_err)})),
                    "OpenAI Codex streaming response decode failed, retrying without streaming"
                );

                request.stream = false;
                let non_streaming_response = self
                    .responses_request_builder(
                        &creds.bearer_token,
                        creds.account_id.as_deref(),
                        creds.access_token.as_deref(),
                        creds.use_gateway_api_key_auth,
                        &request,
                    )
                    .json(&request)
                    .send()
                    .await?;

                if !non_streaming_response.status().is_success() {
                    return Err(super::api_error("OpenAI Codex", non_streaming_response).await);
                }

                decode_responses_body(non_streaming_response)
                    .await
                    .map_err(|fallback_err| {
                        ::zeroclaw_log::record!(
                            ERROR,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                .with_attrs(::serde_json::json!({
                                    "stream_err": format!("{}", stream_err),
                                    "fallback_err": format!("{}", fallback_err),
                                })),
                            "openai_codex: stream + non-stream fallback both failed"
                        );
                        anyhow::Error::msg(format!(
                            "OpenAI Codex streaming response decode failed ({stream_err}); non-streaming retry failed ({fallback_err})"
                        ))
                    })
            }
        }
    }
}

#[async_trait]
impl ModelProvider for OpenAiCodexModelProvider {
    // ── Provider-family defaults ──
    fn default_wire_api(&self) -> &str {
        WIRE_API
    }

    fn default_base_url(&self) -> Option<&str> {
        Some(DEFAULT_CODEX_RESPONSES_URL)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: true,
            vision: true,
            prompt_caching: false,
            extended_thinking: false,
        }
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        // Build temporary messages array
        let mut messages = Vec::new();
        if let Some(sys) = system_prompt {
            messages.push(ChatMessage::system(sys));
        }
        messages.push(ChatMessage::user(message));

        // Normalize images: convert file paths to data URIs, under the
        // configured policy rather than defaults.
        let prepared =
            crate::multimodal::prepare_messages_for_provider(&messages, &self.multimodal).await?;

        let (instructions, input) = build_responses_input(&prepared.messages);
        self.send_responses_request(input, instructions, None, model)
            .await
            .map(|response| response.text.unwrap_or_default())
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        // Normalize image markers: convert file paths to data URIs, under the
        // configured policy rather than defaults.
        let prepared =
            crate::multimodal::prepare_messages_for_provider(messages, &self.multimodal).await?;

        let (instructions, input) = build_responses_input(&prepared.messages);
        self.send_responses_request(input, instructions, None, model)
            .await
            .map(|response| response.text.unwrap_or_default())
    }

    async fn chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<ProviderChatResponse> {
        let prepared =
            crate::multimodal::prepare_messages_for_provider(request.messages, &self.multimodal)
                .await?;
        let (instructions, input) = build_responses_input(&prepared.messages);
        let response = self
            .send_responses_request(input, instructions, convert_tools(request.tools), model)
            .await?;

        Ok(ProviderChatResponse {
            output_blocks: response.output_blocks,
            text: response.text,
            tool_calls: response.tool_calls,
            usage: response.usage,
            reasoning_content: response.reasoning_content,
        })
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
        _temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
        if !options.enabled {
            return stream::once(async { Ok(StreamEvent::Final) }).boxed();
        }

        let provider = self.clone();
        let messages = request.messages.to_vec();
        let tools = request.tools.map(|items| items.to_vec());
        let model = model.to_string();
        let count_tokens = options.count_tokens;
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(100);

        let parser = async move {
            let prepared = match crate::multimodal::prepare_messages_for_provider(
                &messages,
                &provider.multimodal,
            )
            .await
            {
                Ok(prepared) => prepared,
                Err(err) => {
                    let _ = tx
                        .send(Err(StreamError::ModelProvider(err.to_string())))
                        .await;
                    return;
                }
            };

            let creds = match provider.resolve_credentials().await {
                Ok(c) => c,
                Err(err) => {
                    let _ = tx
                        .send(Err(StreamError::ModelProvider(err.to_string())))
                        .await;
                    return;
                }
            };

            let (instructions, input) = build_responses_input(&prepared.messages);
            let normalized_model = normalize_model_id(&model);
            let tools = convert_tools(tools.as_deref());
            let tools_count = tools.as_ref().map_or(0, Vec::len);
            let has_tools = has_turn_tools(tools.as_ref());
            let request = ResponsesRequest {
                model: normalized_model.to_string(),
                input,
                instructions,
                store: false,
                stream: true,
                text: ResponsesTextOptions {
                    verbosity: "medium".to_string(),
                },
                reasoning: ResponsesReasoningOptions {
                    effort: resolve_reasoning_effort(
                        normalized_model,
                        provider.reasoning_effort.as_deref(),
                    ),
                    summary: "auto".to_string(),
                },
                include: vec!["reasoning.encrypted_content".to_string()],
                tools,
                tool_choice: has_tools.then(|| "auto".to_string()),
                parallel_tool_calls: has_tools.then_some(true),
            };
            if ::zeroclaw_log::debug_enabled() {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                        .with_attrs(::serde_json::json!({
                            "provider": "openai_codex",
                            "alias": &provider.alias,
                            "request_api": WIRE_API,
                            "model": &request.model,
                            "stream": true,
                            "tools_count": tools_count,
                            "tool_choice": request.tool_choice.as_deref(),
                            "parallel_tool_calls": request.parallel_tool_calls,
                        })),
                    "openai codex responses streaming provider request prepared"
                );
            }

            let request_builder = provider
                .responses_request_builder(
                    &creds.bearer_token,
                    creds.account_id.as_deref(),
                    creds.access_token.as_deref(),
                    creds.use_gateway_api_key_auth,
                    &request,
                )
                .json(&request);

            crate::openai::run_responses_sse(
                request_builder,
                &tx,
                count_tokens,
                crate::StreamIdleBound::Fixed(crate::STREAM_IDLE_TIMEOUT),
            )
            .await;
        };

        crate::stream_guard::parser_stream(parser, rx)
    }
}

impl ::zeroclaw_api::attribution::Attributable for OpenAiCodexModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::OpenAiCodex,
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

    enum MockCodexReply {
        Sse(&'static str),
        Json(serde_json::Value),
        Status(axum::http::StatusCode, &'static str),
    }

    async fn mock_codex_provider(
        replies: Vec<MockCodexReply>,
    ) -> (
        OpenAiCodexModelProvider,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        tokio::task::JoinHandle<()>,
        tempfile::TempDir,
        crate::RuntimeProxyTestGuard,
    ) {
        use axum::http::header;
        use axum::response::IntoResponse;
        use axum::{Json, Router, routing::post};
        use std::collections::VecDeque;
        use std::sync::{Arc, Mutex};
        use tokio::net::TcpListener;

        let proxy_guard = crate::RuntimeProxyTestGuard::acquire().await;
        let captured: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = Arc::clone(&captured);
        let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
        let replies_clone = Arc::clone(&replies);

        let app = Router::new().route(
            "/responses",
            post(move |Json(body): Json<serde_json::Value>| {
                let captured = Arc::clone(&captured_clone);
                let replies = Arc::clone(&replies_clone);
                async move {
                    captured.lock().unwrap().push(body);
                    match replies
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or(MockCodexReply::Status(
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            "",
                        )) {
                        MockCodexReply::Sse(body) => (
                            axum::http::StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            body.to_string(),
                        )
                            .into_response(),
                        MockCodexReply::Json(body) => Json(body).into_response(),
                        MockCodexReply::Status(status, body) => {
                            (status, body.to_string()).into_response()
                        }
                    }
                }
            }),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_handle = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let temp_dir = tempfile::tempdir().unwrap();
        let options = ModelProviderRuntimeOptions {
            provider_api_url: Some(format!("http://{addr}")),
            zeroclaw_dir: Some(temp_dir.path().to_path_buf()),
            secrets_encrypt: false,
            ..ModelProviderRuntimeOptions::default()
        };
        let provider = OpenAiCodexModelProvider::new("test", &options, Some("test-key")).unwrap();

        (provider, captured, server_handle, temp_dir, proxy_guard)
    }

    #[test]
    fn provider_construction_carries_operator_multimodal_policy() {
        let multimodal = zeroclaw_config::schema::MultimodalConfig {
            max_images: 1,
            max_image_size_mb: 2,
            ..Default::default()
        };

        let options = ModelProviderRuntimeOptions {
            multimodal,
            ..ModelProviderRuntimeOptions::default()
        };
        let provider = OpenAiCodexModelProvider::new("test", &options, None).unwrap();

        // Every `prepare_messages_for_provider` call in this adapter uses this
        // field; defaults would drop the operator's image limits.
        assert_eq!(provider.multimodal.max_images, 1);
        assert_eq!(provider.multimodal.max_image_size_mb, 2);
    }

    #[tokio::test]
    async fn codex_responses_provider_honors_runtime_proxy_after_construction() {
        use axum::{Json, Router, extract::State, routing::post};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use tokio::net::TcpListener;
        use zeroclaw_config::schema::{ProxyConfig, ProxyScope, set_runtime_proxy_config};

        async fn proxy_response(State(hits): State<Arc<AtomicUsize>>) -> Json<serde_json::Value> {
            hits.fetch_add(1, Ordering::SeqCst);
            Json(serde_json::json!({
                "output_text": "proxied",
                "output": []
            }))
        }

        async fn direct_response(State(hits): State<Arc<AtomicUsize>>) -> Json<serde_json::Value> {
            hits.fetch_add(1, Ordering::SeqCst);
            Json(serde_json::json!({
                "output_text": "direct",
                "output": []
            }))
        }

        let _proxy_guard = crate::RuntimeProxyTestGuard::acquire().await;

        let proxy_hits = Arc::new(AtomicUsize::new(0));
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let proxy_app = Router::new()
            .fallback(proxy_response)
            .with_state(Arc::clone(&proxy_hits));
        let proxy_server = zeroclaw_spawn::spawn!(async move {
            axum::serve(proxy_listener, proxy_app).await.unwrap();
        });

        let direct_hits = Arc::new(AtomicUsize::new(0));
        let direct_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let direct_addr = direct_listener.local_addr().unwrap();
        let direct_app = Router::new()
            .route("/responses", post(direct_response))
            .with_state(Arc::clone(&direct_hits));
        let direct_server = zeroclaw_spawn::spawn!(async move {
            axum::serve(direct_listener, direct_app).await.unwrap();
        });

        let temp_dir = tempfile::tempdir().unwrap();
        let options = ModelProviderRuntimeOptions {
            provider_api_url: Some(format!("http://{direct_addr}")),
            zeroclaw_dir: Some(temp_dir.path().to_path_buf()),
            secrets_encrypt: false,
            ..ModelProviderRuntimeOptions::default()
        };
        let provider = OpenAiCodexModelProvider::new("test", &options, Some("test-key")).unwrap();

        set_runtime_proxy_config(ProxyConfig {
            enabled: true,
            http_proxy: Some(format!("http://{proxy_addr}")),
            scope: ProxyScope::Services,
            services: vec!["model_provider.openai".to_string()],
            ..Default::default()
        });

        let request = ResponsesRequest {
            model: "gpt-5".to_string(),
            input: vec![serde_json::json!({
                "role": "user",
                "content": "hello"
            })],
            instructions: DEFAULT_CODEX_INSTRUCTIONS.to_string(),
            store: false,
            stream: true,
            text: ResponsesTextOptions {
                verbosity: "medium".to_string(),
            },
            reasoning: ResponsesReasoningOptions {
                effort: "medium".to_string(),
                summary: "auto".to_string(),
            },
            include: vec!["reasoning.encrypted_content".to_string()],
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
        };
        let request_builder =
            provider.responses_request_builder("test-key", None, None, true, &request);
        set_runtime_proxy_config(ProxyConfig::default());

        let response_body: serde_json::Value = request_builder
            .json(&request)
            .send()
            .await
            .expect("codex Responses request should succeed through runtime proxy")
            .json()
            .await
            .expect("proxy should return a Responses-shaped JSON body");

        proxy_server.abort();
        direct_server.abort();

        assert_eq!(
            response_body
                .get("output_text")
                .and_then(serde_json::Value::as_str),
            Some("proxied")
        );
        assert_eq!(
            proxy_hits.load(Ordering::SeqCst),
            1,
            "runtime proxy server should receive the Codex Responses request"
        );
        assert_eq!(
            direct_hits.load(Ordering::SeqCst),
            0,
            "direct Codex Responses endpoint must not be contacted when runtime proxy applies"
        );
    }

    #[test]
    fn extracts_output_text_first() {
        let response = ResponsesResponse {
            model: None,
            status: None,
            incomplete_details: None,
            output: vec![],
            output_text: Some("hello".into()),
            usage: None,
        };
        assert_eq!(extract_responses_text(&response).as_deref(), Some("hello"));
    }

    #[test]
    fn extracts_nested_output_text() {
        let response = ResponsesResponse {
            model: None,
            status: None,
            incomplete_details: None,
            output: vec![serde_json::json!({
                "type": "message",
                "role": "assistant",
                "content": [
                    {
                        "type": "output_text",
                        "text": "nested"
                    }
                ]
            })],
            output_text: None,
            usage: None,
        };
        assert_eq!(extract_responses_text(&response).as_deref(), Some("nested"));
    }

    #[test]
    fn parses_responses_usage_without_synthesizing_missing_fields() {
        let usage = parse_responses_usage(Some(&serde_json::json!({
            "input_tokens": 120,
            "input_tokens_details": {"cached_tokens": 45},
            "output_tokens": 30
        })))
        .expect("valid usage should be reported");
        assert_eq!(usage.input_tokens, Some(120));
        assert_eq!(usage.cached_input_tokens, Some(45));
        assert_eq!(usage.output_tokens, Some(30));

        let partial = parse_responses_usage(Some(&serde_json::json!({
            "input_tokens": "not-a-number",
            "output_tokens": 7,
            "input_tokens_details": {"cached_tokens": null}
        })))
        .expect("a valid optional field should survive malformed siblings");
        assert_eq!(partial.input_tokens, None);
        assert_eq!(partial.cached_input_tokens, None);
        assert_eq!(partial.output_tokens, Some(7));
        assert!(parse_responses_usage(None).is_none());
        assert!(parse_responses_usage(Some(&serde_json::json!({}))).is_none());
    }

    #[test]
    fn responses_stream_keeps_early_item_and_annotation_metadata() {
        let mut state = ResponsesStreamState::default();
        let mut metadata = Vec::new();
        for frame in [
            serde_json::json!({"type":"response.output_item.added","output_index":2,"item":{"type":"message","id":"msg_fixture","phase":"commentary","status":"in_progress","content":[]}}),
            serde_json::json!({"type":"response.output_text.annotation.added","output_index":2,"content_index":0,"annotation_index":0,"annotation":{"type":"url_citation","url":"https://fixture.invalid"}}),
            serde_json::json!({"type":"response.output_item.done","output_index":3,"item":{"type":"function_call","id":"fc_fixture","call_id":"call_fixture","namespace":"search","name":"lookup","arguments":"{}","status":"completed"}}),
        ] {
            for event in process_responses_stream_event(frame, &mut state).unwrap() {
                match event {
                    StreamEvent::ProtocolMetadata(value)
                    | StreamEvent::OutputBlock(
                        zeroclaw_api::model_provider::ProviderOutputBlock::ProtocolMetadata {
                            metadata: value,
                        },
                    ) => metadata.push(value),
                    _ => {}
                }
            }
        }
        assert_eq!(metadata.len(), 3);
        assert_eq!(metadata[0]["fields"]["phase"], "commentary");
        assert_eq!(metadata[0]["fields"]["status"], "in_progress");
        assert!(metadata[0]["fields"].get("content").is_none());
        assert_eq!(
            metadata[1]["fields"]["annotations"][0]["type"],
            "url_citation"
        );
        assert_eq!(metadata[2]["fields"]["id"], "fc_fixture");
        assert_eq!(metadata[2]["fields"]["namespace"], "search");
    }

    #[test]
    fn completed_stream_event_emits_authoritative_usage() {
        let mut state = ResponsesStreamState::default();
        let events = process_sse_chunk(
            "data: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"done\",\"usage\":{\"input_tokens\":120,\"input_tokens_details\":{\"cached_tokens\":45},\"output_tokens\":30}}}",
            &mut state,
        )
        .expect("completed event should parse");

        assert!(matches!(
            events.as_slice(),
            [
                StreamEvent::ProtocolMetadata(_),
                StreamEvent::Usage(TokenUsage {
                    input_tokens: Some(120),
                    cached_input_tokens: Some(45),
                    cache_creation_input_tokens: None,
                    output_tokens: Some(30),
                })
            ]
        ));
        assert_eq!(
            state.usage.as_ref().and_then(|usage| usage.input_tokens),
            Some(120)
        );
    }

    #[test]
    fn completed_stream_event_omits_usage_when_provider_does_not_report_it() {
        let mut state = ResponsesStreamState::default();
        let events = process_sse_chunk(
            "data: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"done\"}}",
            &mut state,
        )
        .expect("completed event should parse");

        assert!(matches!(
            events.as_slice(),
            [StreamEvent::ProtocolMetadata(_)]
        ));
        assert!(state.usage.is_none());
    }

    #[test]
    fn completed_stream_event_orders_fallback_tool_call_before_usage() {
        let mut state = ResponsesStreamState::default();
        let events = process_sse_chunk(
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"echo\",\"arguments\":\"{}\"}],\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}",
            &mut state,
        )
        .expect("completed event should parse");

        assert!(matches!(
            events.as_slice(),
            [
                StreamEvent::ProtocolMetadata(_),
                StreamEvent::ContentPosition(position),
                StreamEvent::ToolCall(_),
                StreamEvent::Usage(_)
            ] if position.output_index == 0 && position.content_index.is_none()
        ));
    }

    #[test]
    fn done_alias_does_not_emit_usage() {
        let mut state = ResponsesStreamState::default();
        let events = process_sse_chunk(
            "data: {\"type\":\"response.done\",\"response\":{\"output_text\":\"done\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}",
            &mut state,
        )
        .expect("done alias should parse");

        assert!(matches!(
            events.as_slice(),
            [StreamEvent::ProtocolMetadata(_)]
        ));
        assert!(state.usage.is_none());
    }

    #[tokio::test]
    async fn chat_propagates_non_streaming_responses_usage() {
        let (provider, _captured, server_handle, _temp_dir, _proxy_guard) =
            mock_codex_provider(vec![MockCodexReply::Json(serde_json::json!({
                "output_text": "ok",
                "output": [],
                "usage": {
                    "input_tokens": 120,
                    "input_tokens_details": {"cached_tokens": 45},
                    "output_tokens": 30
                }
            }))])
            .await;
        let messages = vec![ChatMessage::user("hello")];

        let response = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "gpt-5-codex",
                None,
            )
            .await
            .expect("chat should succeed");

        let usage = response
            .usage
            .expect("provider-reported usage should propagate");
        assert_eq!(usage.input_tokens, Some(120));
        assert_eq!(usage.cached_input_tokens, Some(45));
        assert_eq!(usage.output_tokens, Some(30));
        server_handle.abort();
    }

    #[test]
    fn has_turn_tools_returns_false_for_empty_and_none() {
        // Pure unit test on the gate helper, complementing the end-to-end
        // `chat()`-based regression below. Asserts the four boundary
        // cases of the `is_some_and(!is_empty())` invariant.
        assert!(!has_turn_tools(None));
        assert!(!has_turn_tools(Some(&vec![])));
        assert!(has_turn_tools(Some(&vec![make_test_tool_spec("echo")])));
        // A non-empty list still passes even when all entries are
        // syntactically distinct from each other; the helper does not
        // dedupe.
        let two = vec![make_test_tool_spec("a"), make_test_tool_spec("b")];
        assert!(has_turn_tools(Some(&two)));
    }

    fn make_test_tool_spec(name: &str) -> ResponsesToolSpec {
        ResponsesToolSpec {
            kind: "function".to_string(),
            name: name.to_string(),
            description: String::new(),
            parameters: serde_json::json!({}).into(),
            strict: false,
        }
    }

    #[tokio::test]
    async fn chat_with_empty_tools_list_omits_tool_choice_and_parallel_tool_calls() {
        let (provider, captured, server_handle, _temp_dir, _proxy_guard) =
            mock_codex_provider(vec![MockCodexReply::Json(serde_json::json!({
                "output_text": "ok",
                "output": []
            }))])
            .await;

        let messages = vec![ChatMessage::user("hello")];
        let empty_tools: Vec<zeroclaw_api::tool::ToolSpec> = vec![];
        let response = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: Some(&empty_tools),
                    thinking: None,
                },
                "gpt-5-codex",
                None,
            )
            .await
            .expect("chat() should succeed with an empty tool list");
        assert_eq!(response.text.as_deref(), Some("ok"));

        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1, "expected exactly one captured request");
        let body = &requests[0];
        assert!(
            body.get("tool_choice").is_none(),
            "empty tools list must produce a request body without `tool_choice`; got: {body}"
        );
        assert!(
            body.get("parallel_tool_calls").is_none(),
            "empty tools list must produce a request body without `parallel_tool_calls`; got: {body}"
        );
        // Sanity: a non-empty tool list still produces both fields.
        assert!(
            body.get("tools").is_none() || body["tools"].as_array().is_none_or(|a| a.is_empty()),
            "empty input list should produce a no-tools request; got: {body}"
        );

        server_handle.abort();
    }

    #[test]
    fn default_state_dir_is_non_empty() {
        let path = default_zeroclaw_dir();
        assert!(!path.as_os_str().is_empty());
    }

    #[test]
    fn build_responses_url_appends_suffix_for_base_url() {
        assert_eq!(
            build_responses_url("https://api.tonsof.blue/v1").unwrap(),
            "https://api.tonsof.blue/v1/responses"
        );
    }

    #[test]
    fn build_responses_url_keeps_existing_responses_endpoint() {
        assert_eq!(
            build_responses_url("https://api.tonsof.blue/v1/responses").unwrap(),
            "https://api.tonsof.blue/v1/responses"
        );
    }

    #[test]
    fn resolve_responses_url_uses_provider_api_url_override() {
        let options = ModelProviderRuntimeOptions {
            provider_api_url: Some("https://proxy.example.com/v1".to_string()),
            ..ModelProviderRuntimeOptions::default()
        };

        assert_eq!(
            resolve_responses_url(&options).unwrap(),
            "https://proxy.example.com/v1/responses"
        );
    }

    #[test]
    fn default_responses_url_detector_handles_equivalent_urls() {
        assert!(is_default_responses_url(DEFAULT_CODEX_RESPONSES_URL));
        assert!(is_default_responses_url(
            "https://chatgpt.com/backend-api/codex/responses/"
        ));
        assert!(!is_default_responses_url(
            "https://api.tonsof.blue/v1/responses"
        ));
    }

    #[test]
    fn constructor_enables_custom_endpoint_key_mode() {
        let options = ModelProviderRuntimeOptions {
            provider_api_url: Some("https://api.tonsof.blue/v1".to_string()),
            ..ModelProviderRuntimeOptions::default()
        };

        let provider = OpenAiCodexModelProvider::new("test", &options, Some("test-key")).unwrap();
        assert!(provider.custom_endpoint);
        assert_eq!(provider.gateway_api_key.as_deref(), Some("test-key"));
    }

    #[tokio::test]
    async fn codex_retries_non_streaming_when_stream_decode_fails() {
        let (provider, captured, server_handle, _temp_dir, _proxy_guard) =
            mock_codex_provider(vec![
                MockCodexReply::Sse("data: not-json\n\ndata: [DONE]\n"),
                MockCodexReply::Json(serde_json::json!({
                    "output_text": "fallback ok",
                    "output": []
                })),
            ])
            .await;

        let messages = vec![ChatMessage::user("hello")];
        let response = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "gpt-5-codex",
                None,
            )
            .await
            .expect("provider should retry with stream=false after streaming decode failure");

        assert_eq!(response.text.as_deref(), Some("fallback ok"));

        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 2, "expected one retry request");
        assert_eq!(requests[0]["stream"], true);
        assert_eq!(requests[1]["stream"], false);

        server_handle.abort();
    }

    #[tokio::test]
    async fn codex_retries_non_streaming_when_stream_contains_malformed_frame_after_text() {
        let (provider, captured, server_handle, _temp_dir, _proxy_guard) = mock_codex_provider(vec![
            MockCodexReply::Sse(
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\ndata: not-json\n\ndata: [DONE]\n",
            ),
            MockCodexReply::Json(serde_json::json!({
                "output_text": "fallback after partial",
                "output": []
            })),
        ])
        .await;

        let messages = vec![ChatMessage::user("hello")];
        let response = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "gpt-5-codex",
                None,
            )
            .await
            .expect("provider should retry after malformed stream frame");

        assert_eq!(response.text.as_deref(), Some("fallback after partial"));

        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 2, "expected one retry request");
        assert_eq!(requests[0]["stream"], true);
        assert_eq!(requests[1]["stream"], false);

        server_handle.abort();
    }

    #[tokio::test]
    async fn codex_does_not_retry_stream_api_error_events() {
        let (provider, captured, server_handle, _temp_dir, _proxy_guard) = mock_codex_provider(vec![
            MockCodexReply::Sse(
                "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"quota exceeded\"}}}\n\ndata: [DONE]\n",
            ),
        ])
        .await;

        let messages = vec![ChatMessage::user("hello")];
        let err = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "gpt-5-codex",
                None,
            )
            .await
            .expect_err("stream API errors should not be retried");

        assert!(err.to_string().contains("quota exceeded"), "{err}");
        assert_eq!(captured.lock().unwrap().len(), 1);

        server_handle.abort();
    }

    #[tokio::test]
    async fn codex_does_not_retry_failed_http_status() {
        let (provider, captured, server_handle, _temp_dir, _proxy_guard) =
            mock_codex_provider(vec![MockCodexReply::Status(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "server down",
            )])
            .await;

        let messages = vec![ChatMessage::user("hello")];
        provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "gpt-5-codex",
                None,
            )
            .await
            .expect_err("HTTP errors should not be retried");

        assert_eq!(captured.lock().unwrap().len(), 1);

        server_handle.abort();
    }

    #[test]
    fn clamp_reasoning_effort_adjusts_known_models() {
        assert_eq!(
            clamp_reasoning_effort("gpt-5-codex", "xhigh"),
            "high".to_string()
        );
        assert_eq!(
            clamp_reasoning_effort("gpt-5-codex", "minimal"),
            "low".to_string()
        );
        assert_eq!(
            clamp_reasoning_effort("gpt-5-codex", "medium"),
            "medium".to_string()
        );
        assert_eq!(
            clamp_reasoning_effort("gpt-5.3-codex", "minimal"),
            "low".to_string()
        );
        assert_eq!(
            clamp_reasoning_effort("gpt-5.1", "xhigh"),
            "high".to_string()
        );
        assert_eq!(
            clamp_reasoning_effort("gpt-5-codex", "xhigh"),
            "high".to_string()
        );
        assert_eq!(
            clamp_reasoning_effort("gpt-5.1-codex-mini", "low"),
            "medium".to_string()
        );
        assert_eq!(
            clamp_reasoning_effort("gpt-5.1-codex-mini", "xhigh"),
            "high".to_string()
        );
        assert_eq!(
            clamp_reasoning_effort("gpt-5.3-codex", "xhigh"),
            "xhigh".to_string()
        );
    }

    #[test]
    fn resolve_reasoning_effort_prefers_configured_override() {
        // V0.8.0 grammar: configured value wins; no env-var fallback.
        assert_eq!(
            resolve_reasoning_effort("gpt-5-codex", Some("high")),
            "high".to_string()
        );
    }

    #[test]
    fn resolve_reasoning_effort_defaults_when_unconfigured() {
        assert_eq!(
            resolve_reasoning_effort("gpt-5-codex", None),
            "high".to_string()
        );
    }

    #[test]
    fn parse_sse_turn_reads_output_text_delta() {
        let payload = r#"data: {"type":"response.created","response":{"id":"resp_123"}}

data: {"type":"response.output_text.delta","delta":"Hello"}
data: {"type":"response.output_text.delta","delta":" world"}
data: {"type":"response.completed","response":{"output_text":"Hello world"}}
data: [DONE]
"#;

        assert_eq!(
            parse_sse_turn(payload).unwrap().text.as_deref(),
            Some("Hello world")
        );
    }

    #[test]
    fn process_sse_chunk_marks_completion_on_response_completed() {
        let mut state = ResponsesStreamState::default();
        let _ = process_sse_chunk(
            "data: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"hi\"}}",
            &mut state,
        )
        .unwrap();
        assert!(state.saw_completion);
    }

    #[test]
    fn process_sse_chunk_marks_completion_on_done_sentinel() {
        let mut state = ResponsesStreamState::default();
        let _ = process_sse_chunk("data: [DONE]", &mut state).unwrap();
        assert!(state.saw_completion);
    }

    #[test]
    fn process_sse_chunk_leaves_completion_unset_on_text_delta() {
        let mut state = ResponsesStreamState::default();
        let _ = process_sse_chunk(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}",
            &mut state,
        )
        .unwrap();
        assert!(!state.saw_completion);
    }

    #[test]
    fn parse_sse_turn_falls_back_to_completed_response() {
        let payload = r#"data: {"type":"response.completed","response":{"output_text":"Done"}}
data: [DONE]
"#;

        assert_eq!(
            parse_sse_turn(payload).unwrap().text.as_deref(),
            Some("Done")
        );
    }

    #[test]
    fn parse_responses_body_rejects_unrecognized_sse_without_payload() {
        let payload = r#"data: not-json
data: [DONE]
"#;

        let err = parse_responses_body(payload).expect_err("empty SSE should fail closed");
        assert!(
            err.to_string()
                .contains("OpenAI Codex SSE data parse failed"),
            "{err}"
        );
    }

    #[test]
    fn parse_responses_body_rejects_json_without_text_or_tool_calls() {
        let payload = r#"{"output":[]}"#;

        let err = parse_responses_body(payload).expect_err("empty JSON should fail closed");
        assert!(
            err.to_string().contains("No response from OpenAI Codex"),
            "{err}"
        );
    }

    #[test]
    fn parse_responses_body_allows_sse_markers_inside_json_text() {
        let payload = serde_json::json!({
            "output_text": "Example SSE frame:\ndata: {\"type\":\"example\"}\nevent: response.done",
            "output": []
        })
        .to_string();

        let result = parse_responses_body(&payload).expect("JSON text should not be parsed as SSE");
        assert_eq!(
            result.text.as_deref(),
            Some("Example SSE frame:\ndata: {\"type\":\"example\"}\nevent: response.done")
        );
        assert!(result.tool_calls.is_empty());
    }

    #[test]
    fn parse_responses_body_preserves_reasoning_items_for_tool_calls() {
        let payload = serde_json::json!({
            "output": [
                {
                    "type": "reasoning",
                    "id": "rs_1",
                    "summary": [],
                    "encrypted_content": "enc_reasoning"
                },
                {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "shell",
                    "arguments": "{\"command\":\"pwd\"}",
                    "status": "completed"
                }
            ]
        })
        .to_string();

        let result = parse_responses_body(&payload).expect("tool call response should parse");
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].id, "call_1");

        let items = decode_responses_history_items(
            result
                .reasoning_content
                .as_deref()
                .expect("Responses history items should be captured"),
        )
        .expect("history envelope should decode");

        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["type"], "reasoning");
        assert_eq!(items[0]["encrypted_content"], "enc_reasoning");
        assert_eq!(items[1]["type"], "function_call");
        assert_eq!(items[1]["call_id"], "call_1");
    }

    #[test]
    fn build_responses_input_maps_content_types_by_role() {
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: "You are helpful.".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Hi".into(),
            },
            ChatMessage {
                role: "assistant".into(),
                content: "Hello!".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Thanks".into(),
            },
        ];
        let (instructions, input) = build_responses_input(&messages);
        assert_eq!(instructions, "You are helpful.");
        assert_eq!(input.len(), 3);

        let json: Vec<Value> = input
            .iter()
            .map(|item| serde_json::to_value(item).unwrap())
            .collect();
        assert_eq!(json[0]["role"], "user");
        assert_eq!(json[0]["content"][0]["type"], "input_text");
        assert_eq!(json[1]["role"], "assistant");
        assert_eq!(json[1]["content"][0]["type"], "output_text");
        assert_eq!(json[2]["role"], "user");
        assert_eq!(json[2]["content"][0]["type"], "input_text");
    }

    #[test]
    fn build_responses_input_uses_default_instructions_without_system() {
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: "Hello".into(),
        }];
        let (instructions, input) = build_responses_input(&messages);
        assert_eq!(instructions, DEFAULT_CODEX_INSTRUCTIONS);
        assert_eq!(input.len(), 1);
    }

    #[test]
    fn build_responses_input_maps_tool_outputs() {
        let messages = vec![
            ChatMessage {
                role: "tool".into(),
                content: r#"{"tool_call_id":"call_123","content":"result"}"#.into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Go".into(),
            },
        ];
        let (instructions, input) = build_responses_input(&messages);
        assert_eq!(instructions, DEFAULT_CODEX_INSTRUCTIONS);
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["type"], "function_call_output");
        assert_eq!(input[0]["call_id"], "call_123");
        assert_eq!(input[0]["output"], "result");
        assert_eq!(input[1]["role"], "user");
    }

    #[test]
    fn build_responses_input_replays_plain_tool_text_without_synthetic_call_id() {
        let messages = vec![ChatMessage {
            role: "tool".into(),
            content: "legacy plain text result".into(),
        }];

        let (_, input) = build_responses_input(&messages);

        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(
            input[0]["content"][0]["text"],
            "Legacy tool output without call_id:\nlegacy plain text result"
        );
        assert!(input[0].get("call_id").is_none());
    }

    #[test]
    fn build_responses_input_replays_tool_json_without_call_id_as_text() {
        let messages = vec![ChatMessage {
            role: "tool".into(),
            content: r#"{"content":"legacy result","status":"ok"}"#.into(),
        }];

        let (_, input) = build_responses_input(&messages);

        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(
            input[0]["content"][0]["text"],
            r#"Legacy tool output without call_id:
{"content":"legacy result","status":"ok"}"#
        );
        assert!(input[0].get("call_id").is_none());
    }

    #[test]
    fn build_responses_input_replays_blank_tool_call_id_as_legacy_text() {
        for raw_id in ["", "   "] {
            let messages = vec![ChatMessage {
                role: "tool".into(),
                content: serde_json::json!({
                    "tool_call_id": raw_id,
                    "content": "legacy result"
                })
                .to_string(),
            }];

            let (_, input) = build_responses_input(&messages);

            assert_eq!(input.len(), 1);
            assert_eq!(input[0]["type"], "message");
            assert_eq!(input[0]["role"], "user");
            assert_eq!(input[0]["content"][0]["type"], "input_text");
            assert!(input[0].get("call_id").is_none());
            assert!(
                input[0]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("\"legacy result\"")
            );
        }
    }

    #[test]
    fn build_responses_input_maps_native_assistant_tool_calls() {
        let messages = vec![ChatMessage::assistant(
            r#"{"content":"Using shell","tool_calls":[{"id":"call_abc","name":"shell","arguments":"{\"command\":\"pwd\"}"}]}"#,
        )];
        let (_, input) = build_responses_input(&messages);

        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "assistant");
        assert_eq!(input[0]["content"][0]["type"], "output_text");
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["call_id"], "call_abc");
        assert_eq!(input[1]["name"], "shell");
    }

    #[test]
    fn build_responses_input_replays_reasoning_item_before_tool_result() {
        let reasoning_item = serde_json::json!({
            "type": "reasoning",
            "id": "rs_1",
            "summary": [],
            "encrypted_content": "enc_reasoning"
        });
        let function_call_item = serde_json::json!({
            "type": "function_call",
            "id": "fc_1",
            "call_id": "call_1",
            "name": "shell",
            "arguments": "{\"command\":\"pwd\"}",
            "status": "completed"
        });
        let reasoning_content =
            encode_responses_history_items(&[reasoning_item, function_call_item], true)
                .expect("history envelope should encode");
        let messages = vec![
            ChatMessage::assistant(
                serde_json::json!({
                    "content": null,
                    "tool_calls": [
                        {
                            "id": "call_1",
                            "name": "shell",
                            "arguments": "{\"command\":\"pwd\"}"
                        }
                    ],
                    "reasoning_content": reasoning_content
                })
                .to_string(),
            ),
            ChatMessage::tool(
                serde_json::json!({
                    "tool_call_id": "call_1",
                    "content": "ok"
                })
                .to_string(),
            ),
        ];

        let (_, input) = build_responses_input(&messages);
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[0]["encrypted_content"], "enc_reasoning");
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["call_id"], "call_1");
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["call_id"], "call_1");
        assert_eq!(input[2]["output"], "ok");
    }

    #[test]
    fn convert_tools_opts_out_of_responses_strict_mode() {
        let tools = vec![ToolSpec::new(
            "jira",
            "Interact with Jira",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string" },
                    "issue_key": { "type": "string" }
                },
                "required": ["action"]
            }),
        )];

        let converted = convert_tools(Some(&tools)).expect("tool should convert");
        let value = serde_json::to_value(&converted[0]).expect("tool should serialize");
        assert_eq!(value["type"], "function");
        assert_eq!(value["name"], "jira");
        assert_eq!(value["strict"], false);
        assert_eq!(value["parameters"]["required"][0], "action");
    }

    #[test]
    fn parse_sse_turn_collects_function_calls() {
        let payload = r#"data: {"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"shell","arguments":""}}

data: {"type":"response.function_call_arguments.delta","item_id":"fc_1","output_index":0,"delta":"{\"command\":\"pw"}
data: {"type":"response.function_call_arguments.done","item_id":"fc_1","output_index":0,"name":"shell","arguments":"{\"command\":\"pwd\"}"}
data: {"type":"response.completed","response":{"output":[]}}
data: [DONE]
"#;

        let result = parse_sse_turn(payload).unwrap();
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].id, "call_1");
        assert_eq!(result.tool_calls[0].name, "shell");
        assert_eq!(result.tool_calls[0].arguments, "{\"command\":\"pwd\"}");
    }

    #[test]
    fn build_responses_input_handles_image_markers() {
        let messages = vec![ChatMessage::user(
            "Describe this\n\n[IMAGE:data:image/png;base64,abc]",
        )];
        let (_, input) = build_responses_input(&messages);

        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"].as_array().unwrap().len(), 2);

        let json = input[0]["content"].as_array().unwrap();

        // First content = text
        assert_eq!(json[0]["type"], "input_text");
        assert!(json[0]["text"].as_str().unwrap().contains("Describe this"));

        // Second content = image
        assert_eq!(json[1]["type"], "input_image");
        assert_eq!(json[1]["image_url"], "data:image/png;base64,abc");
    }

    #[test]
    fn build_responses_input_preserves_text_only_messages() {
        let messages = vec![ChatMessage::user("Hello without images")];
        let (_, input) = build_responses_input(&messages);

        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["content"].as_array().unwrap().len(), 1);

        let json = &input[0]["content"][0];
        assert_eq!(json["type"], "input_text");
        assert_eq!(json["text"], "Hello without images");
    }

    #[test]
    fn build_responses_input_handles_multiple_images() {
        let messages = vec![ChatMessage::user(
            "Compare these: [IMAGE:data:image/png;base64,img1] and [IMAGE:data:image/jpeg;base64,img2]",
        )];
        let (_, input) = build_responses_input(&messages);

        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["content"].as_array().unwrap().len(), 3); // text + 2 images

        let json = input[0]["content"].as_array().unwrap();

        assert_eq!(json[0]["type"], "input_text");
        assert_eq!(json[1]["type"], "input_image");
        assert_eq!(json[2]["type"], "input_image");
    }

    #[test]
    fn capabilities_includes_vision() {
        let options = ModelProviderRuntimeOptions {
            secrets_encrypt: false,
            ..ModelProviderRuntimeOptions::default()
        };
        let provider = OpenAiCodexModelProvider::new("test", &options, None)
            .expect("provider should initialize");
        let caps = provider.capabilities();

        assert!(caps.native_tool_calling);
        assert!(caps.vision);
    }

    #[test]
    fn provider_advertises_streaming_tool_events() {
        let provider =
            OpenAiCodexModelProvider::new("test", &ModelProviderRuntimeOptions::default(), None)
                .expect("provider should initialize");

        assert!(provider.supports_streaming());
        assert!(provider.supports_streaming_tool_events());
    }

    #[test]
    fn build_function_call_item_sanitizes_invalid_arguments_to_empty_object() {
        // Pins that the openai_codex call site of `sanitize_tool_arguments`
        // is wired in. The helper contract itself is covered in
        // `compatible::tests::sanitize_tool_arguments_*`.
        let call = ProviderToolCall {
            id: "call_bad".to_string(),
            name: "shell".to_string(),
            arguments: r#"{"command":"rm -rf"#.to_string(),
            extra_content: None,
        };

        let item = build_function_call_item(call);
        assert_eq!(item["type"], "function_call");
        assert_eq!(item["call_id"], "call_bad");
        assert_eq!(item["name"], "shell");
        assert_eq!(item["arguments"], "{}");
    }

    #[test]
    fn build_function_call_item_passes_through_valid_arguments() {
        // Companion regression: valid JSON must round-trip byte-for-byte.
        let call = ProviderToolCall {
            id: "call_ok".to_string(),
            name: "shell".to_string(),
            arguments: r#"{"command":"pwd"}"#.to_string(),
            extra_content: None,
        };

        let item = build_function_call_item(call);
        assert_eq!(item["arguments"], r#"{"command":"pwd"}"#);
    }
}
