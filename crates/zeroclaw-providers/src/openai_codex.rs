use crate::ModelProviderRuntimeOptions;
use crate::auth::AuthService;
use crate::auth::openai_oauth::extract_account_id_from_jwt;
use crate::multimodal;
use crate::stream_guard::AbortOnDrop;
use crate::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    ModelProvider, ProviderCapabilities, StreamChunk, StreamError, StreamEvent, StreamOptions,
    StreamResult, ToolCall as ProviderToolCall,
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
    client: Client,
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
    pub(crate) parameters: Value,
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
    output: Vec<Value>,
    #[serde(default)]
    output_text: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct ResponsesStreamState {
    pub(crate) saw_text_delta: bool,
    pub(crate) text_accumulator: String,
    pub(crate) fallback_text: Option<String>,
    pub(crate) tool_calls: HashMap<String, PendingToolCall>,
    pub(crate) emitted_tool_call_ids: HashSet<String>,
    pub(crate) collected_tool_calls: Vec<ProviderToolCall>,
    pub(crate) output_items: Vec<Value>,
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
    pub(crate) text: Option<String>,
    pub(crate) tool_calls: Vec<ProviderToolCall>,
    pub(crate) reasoning_content: Option<String>,
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
            client: Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .read_timeout(std::time::Duration::from_secs(300))
                .build()
                .unwrap_or_else(|_| Client::new()),
        })
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
                parameters: tool.parameters.clone(),
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

pub(crate) fn build_responses_input(messages: &[ChatMessage]) -> (String, Vec<Value>) {
    let mut system_parts: Vec<&str> = Vec::new();
    let mut input: Vec<Value> = Vec::new();

    for msg in messages {
        match msg.role.as_str() {
            "system" => system_parts.push(&msg.content),
            "user" => {
                let (cleaned_text, image_refs) = multimodal::parse_image_markers(&msg.content);

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
                        input.push(serde_json::json!({
                            "type": "function_call",
                            "call_id": call.id,
                            "name": call.name,
                            "arguments": call.arguments,
                        }));
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
    if let Some(text) = first_nonempty(response.output_text.as_deref()) {
        return Some(text);
    }

    for item in &response.output {
        if response_item_type(item) != Some("message") {
            continue;
        }

        if let Some(parts) = item.get("content").and_then(Value::as_array) {
            for content in parts {
                if response_item_type(content) == Some("output_text")
                    && let Some(text) = first_nonempty(content.get("text").and_then(Value::as_str))
                {
                    return Some(text);
                }
            }
        }
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
        .filter(|item| response_item_type(item) == Some("function_call"))
        .filter_map(|item| {
            let name = item.get("name").and_then(Value::as_str)?.to_string();
            let arguments = item
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Some(ProviderToolCall {
                id: item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("id").and_then(Value::as_str))
                    .map(ToString::to_string)
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                name,
                arguments,
                extra_content: None,
            })
        })
        .collect()
}

fn responses_turn_from_response(response: &ResponsesResponse) -> ResponsesTurnResult {
    let tool_calls = extract_responses_tool_calls(response);
    let reasoning_content =
        encode_responses_history_items(&response.output, !tool_calls.is_empty());

    ResponsesTurnResult {
        text: extract_responses_text(response),
        tool_calls,
        reasoning_content,
    }
}

fn record_responses_output_item(state: &mut ResponsesStreamState, item: Value) {
    if !is_replayable_responses_output_item(&item) {
        return;
    }

    let item_id = item
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| item.get("call_id").and_then(Value::as_str));
    if let Some(item_id) = item_id
        && state.output_items.iter().any(|existing| {
            existing
                .get("id")
                .and_then(Value::as_str)
                .or_else(|| existing.get("call_id").and_then(Value::as_str))
                == Some(item_id)
        })
    {
        return;
    }

    state.output_items.push(item);
}

fn replace_responses_output_items(state: &mut ResponsesStreamState, items: &[Value]) {
    let replay_items = items
        .iter()
        .filter(|item| is_replayable_responses_output_item(item))
        .cloned()
        .collect::<Vec<_>>();

    if !replay_items.is_empty() {
        state.output_items = replay_items;
    }
}

fn response_output_text_from_event_item(item: &Value) -> Option<String> {
    if item.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }

    item.get("content")
        .and_then(Value::as_array)
        .and_then(|parts| {
            parts.iter().find_map(|part| {
                if part.get("type").and_then(Value::as_str) == Some("output_text") {
                    first_nonempty(part.get("text").and_then(Value::as_str))
                } else {
                    None
                }
            })
        })
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

#[derive(Debug)]
pub(crate) struct ResponsesStreamApiError(pub(crate) String);

impl std::fmt::Display for ResponsesStreamApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OpenAI responses stream error: {}", self.0)
    }
}

impl std::error::Error for ResponsesStreamApiError {}

pub(crate) fn process_responses_stream_event(
    event: Value,
    state: &mut ResponsesStreamState,
) -> anyhow::Result<Vec<StreamEvent>> {
    if let Some(message) = extract_stream_error_message(&event) {
        return Err(ResponsesStreamApiError(message).into());
    }

    let mut emitted = Vec::new();
    match event.get("type").and_then(Value::as_str) {
        Some("response.output_text.delta") => {
            if let Some(text) = nonempty_preserve(event.get("delta").and_then(Value::as_str)) {
                state.saw_text_delta = true;
                state.text_accumulator.push_str(&text);
                emitted.push(StreamEvent::TextDelta(StreamChunk::delta(text)));
            }
        }
        Some("response.output_text.done") if !state.saw_text_delta => {
            state.fallback_text = nonempty_preserve(event.get("text").and_then(Value::as_str));
        }
        Some("response.output_item.added") => {
            let item = event.get("item");
            let item_type = item
                .and_then(|value| value.get("type"))
                .and_then(Value::as_str);
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
                    emitted.push(StreamEvent::ToolCall(tool_call));
                }
            }
        }
        Some("response.output_item.done") => {
            if let Some(item) = event.get("item") {
                record_responses_output_item(state, item.clone());
                match item.get("type").and_then(Value::as_str) {
                    Some("message") if !state.saw_text_delta && state.fallback_text.is_none() => {
                        state.fallback_text = response_output_text_from_event_item(item);
                    }
                    Some("function_call") => {
                        if let Some(name) = item.get("name").and_then(Value::as_str) {
                            let tool_call = ProviderToolCall {
                                id: item
                                    .get("call_id")
                                    .and_then(Value::as_str)
                                    .or_else(|| item.get("id").and_then(Value::as_str))
                                    .unwrap_or_default()
                                    .to_string(),
                                name: name.to_string(),
                                arguments: item
                                    .get("arguments")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                                extra_content: None,
                            };
                            if let Some(tool_call) = emit_tool_call(state, tool_call) {
                                emitted.push(StreamEvent::ToolCall(tool_call));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Some("response.completed" | "response.done") => {
            if let Some(response) = event
                .get("response")
                .and_then(|value| serde_json::from_value::<ResponsesResponse>(value.clone()).ok())
            {
                if !state.saw_text_delta && state.fallback_text.is_none() {
                    state.fallback_text = extract_responses_text(&response);
                }
                replace_responses_output_items(state, &response.output);
                for tool_call in extract_responses_tool_calls(&response) {
                    if let Some(tool_call) = emit_tool_call(state, tool_call) {
                        emitted.push(StreamEvent::ToolCall(tool_call));
                    }
                }
            }
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
        return Ok(Vec::new());
    }

    if let Ok(event) = serde_json::from_str::<Value>(trimmed) {
        return process_responses_stream_event(event, state);
    }

    let mut emitted = Vec::new();
    for line in data_lines {
        let line = line.trim();
        if line.is_empty() || line == "[DONE]" {
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
            anyhow::Error::msg(format!(
                "OpenAI Codex SSE data parse failed: {err}. Payload: {sanitized}"
            ))
        })?;
        emitted.extend(process_responses_stream_event(event, state)?);
    }

    Ok(emitted)
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

    Ok(ResponsesTurnResult {
        text: if state.saw_text_delta {
            nonempty_preserve(Some(&state.text_accumulator))
        } else {
            state.fallback_text
        },
        reasoning_content: encode_responses_history_items(
            &state.output_items,
            !state.collected_tool_calls.is_empty(),
        ),
        tool_calls: state.collected_tool_calls,
    })
}

fn ensure_nonempty_responses_turn(
    result: ResponsesTurnResult,
    empty_error: impl FnOnce() -> anyhow::Error,
) -> anyhow::Result<ResponsesTurnResult> {
    if result.text.as_deref().is_some_and(|text| !text.is_empty()) || !result.tool_calls.is_empty()
    {
        Ok(result)
    } else {
        Err(empty_error())
    }
}

pub(crate) fn extract_stream_error_message(event: &Value) -> Option<String> {
    let event_type = event.get("type").and_then(Value::as_str);

    if event_type == Some("error") {
        return first_nonempty(
            event
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| event.get("code").and_then(Value::as_str))
                .or_else(|| {
                    event
                        .get("error")
                        .and_then(|error| error.get("message"))
                        .and_then(Value::as_str)
                }),
        );
    }

    if event_type == Some("response.failed") {
        return first_nonempty(
            event
                .get("response")
                .and_then(|response| response.get("error"))
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str),
        );
    }

    None
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
                return Err(anyhow::Error::msg(format!(
                    "OpenAI Codex response contained invalid UTF-8: {err}"
                )));
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
        anyhow::Error::msg(format!(
            "OpenAI Codex JSON parse failed: {err}. Payload: {sanitized}"
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

/// Read the response body incrementally via `bytes_stream()` to avoid
/// buffering the entire SSE payload in memory.  The previous implementation
/// used `response.text().await?` which holds the HTTP connection open until
/// every byte has arrived — on high-latency links the long-lived connection
/// often drops mid-read, producing the "error decoding response body" failure
/// reported in #3544.
async fn decode_responses_body(response: reqwest::Response) -> anyhow::Result<ResponsesTurnResult> {
    let mut body = String::new();
    let mut pending_utf8 = Vec::new();
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
            anyhow::Error::msg(format!("error reading OpenAI Codex response stream: {err}"))
        })?;
        append_utf8_stream_chunk(&mut body, &mut pending_utf8, &bytes)?;
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
        return Err(anyhow::Error::msg(format!(
            "OpenAI Codex response ended with incomplete UTF-8: {err}"
        )));
    }

    parse_responses_body(&body)
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

        let account_id = profile.and_then(|p| p.account_id).or_else(|| {
            oauth_access_token
                .as_deref()
                .and_then(extract_account_id_from_jwt)
        });

        let access_token = if use_gateway_api_key_auth {
            oauth_access_token
        } else {
            Some(oauth_access_token.ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"missing": "oauth_access_token"})),
                    "openai_codex: auth profile not found"
                );
                anyhow::Error::msg(
                    "OpenAI Codex auth profile not found. Run `zeroclaw auth login --provider openai-codex`.",
                )
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
                    "OpenAI Codex account id not found in auth profile/token. Run `zeroclaw auth login --provider openai-codex` again.",
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
            .client
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

        let has_tools = tools.is_some();
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

        // Normalize images: convert file paths to data URIs
        let config = zeroclaw_config::schema::MultimodalConfig::default();
        let prepared = crate::multimodal::prepare_messages_for_provider(&messages, &config).await?;

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
        // Normalize image markers: convert file paths to data URIs
        let config = zeroclaw_config::schema::MultimodalConfig::default();
        let prepared = crate::multimodal::prepare_messages_for_provider(messages, &config).await?;

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
        let config = zeroclaw_config::schema::MultimodalConfig::default();
        let prepared =
            crate::multimodal::prepare_messages_for_provider(request.messages, &config).await?;
        let (instructions, input) = build_responses_input(&prepared.messages);
        let response = self
            .send_responses_request(input, instructions, convert_tools(request.tools), model)
            .await?;

        Ok(ProviderChatResponse {
            text: response.text,
            tool_calls: response.tool_calls,
            usage: None,
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

        let handle = ::zeroclaw_spawn::spawn!(async move {
            let config = zeroclaw_config::schema::MultimodalConfig::default();
            let prepared =
                match crate::multimodal::prepare_messages_for_provider(&messages, &config).await {
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
            let has_tools = tools.is_some();
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

            let request_builder = provider
                .responses_request_builder(
                    &creds.bearer_token,
                    creds.account_id.as_deref(),
                    creds.access_token.as_deref(),
                    creds.use_gateway_api_key_auth,
                    &request,
                )
                .json(&request);

            crate::openai::run_responses_sse(request_builder, &tx, count_tokens).await;
        });

        let guard = AbortOnDrop::new(handle.abort_handle());
        stream::unfold((rx, guard), |(mut rx, guard)| async move {
            rx.recv().await.map(|event| (event, (rx, guard)))
        })
        .boxed()
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
    ) {
        use axum::http::header;
        use axum::response::IntoResponse;
        use axum::{Json, Router, routing::post};
        use std::collections::VecDeque;
        use std::sync::{Arc, Mutex};
        use tokio::net::TcpListener;

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

        (provider, captured, server_handle, temp_dir)
    }

    #[test]
    fn extracts_output_text_first() {
        let response = ResponsesResponse {
            output: vec![],
            output_text: Some("hello".into()),
        };
        assert_eq!(extract_responses_text(&response).as_deref(), Some("hello"));
    }

    #[test]
    fn extracts_nested_output_text() {
        let response = ResponsesResponse {
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
        };
        assert_eq!(extract_responses_text(&response).as_deref(), Some("nested"));
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
        let (provider, captured, server_handle, _temp_dir) = mock_codex_provider(vec![
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
        let (provider, captured, server_handle, _temp_dir) = mock_codex_provider(vec![
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
        let (provider, captured, server_handle, _temp_dir) = mock_codex_provider(vec![
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

        assert!(
            err.to_string()
                .contains("OpenAI responses stream error: quota exceeded"),
            "{err}"
        );
        assert_eq!(captured.lock().unwrap().len(), 1);

        server_handle.abort();
    }

    #[tokio::test]
    async fn codex_does_not_retry_failed_http_status() {
        let (provider, captured, server_handle, _temp_dir) =
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
        let tools = vec![ToolSpec {
            name: "jira".to_string(),
            description: "Interact with Jira".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string" },
                    "issue_key": { "type": "string" }
                },
                "required": ["action"]
            }),
        }];

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
}
