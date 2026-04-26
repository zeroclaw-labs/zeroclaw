use crate::ProviderRuntimeOptions;
use crate::auth::AuthService;
use crate::auth::openai_oauth::extract_account_id_from_jwt;
use crate::multimodal;
use crate::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    Provider, ProviderCapabilities, StreamChunk, StreamError, StreamEvent, StreamOptions,
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
const CODEX_RESPONSES_URL_ENV: &str = "ZEROCLAW_CODEX_RESPONSES_URL";
const CODEX_BASE_URL_ENV: &str = "ZEROCLAW_CODEX_BASE_URL";
const DEFAULT_CODEX_INSTRUCTIONS: &str =
    "You are ZeroClaw, a concise and helpful coding assistant.";
/// OpenAI Codex speaks the "responses" wire protocol, not chat_completions.
const WIRE_API: &str = "responses";

#[derive(Clone)]
pub struct OpenAiCodexProvider {
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
struct ResponsesToolSpec {
    #[serde(rename = "type")]
    kind: String,
    name: String,
    description: String,
    parameters: Value,
    strict: bool,
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
    output: Vec<ResponsesOutput>,
    #[serde(default)]
    output_text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponsesOutput {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
    #[serde(default)]
    content: Vec<ResponsesContent>,
}

#[derive(Debug, Deserialize)]
struct ResponsesContent {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Default)]
struct ResponsesStreamState {
    saw_text_delta: bool,
    text_accumulator: String,
    fallback_text: Option<String>,
    tool_calls: HashMap<String, PendingToolCall>,
    emitted_tool_call_ids: HashSet<String>,
    collected_tool_calls: Vec<ProviderToolCall>,
}

#[derive(Debug, Default, Clone)]
struct PendingToolCall {
    item_id: Option<String>,
    call_id: Option<String>,
    name: Option<String>,
    arguments: String,
}

#[derive(Debug, Default)]
struct ResponsesTurnResult {
    text: Option<String>,
    tool_calls: Vec<ProviderToolCall>,
}

impl OpenAiCodexProvider {
    pub fn new(
        options: &ProviderRuntimeOptions,
        gateway_api_key: Option<&str>,
    ) -> anyhow::Result<Self> {
        let state_dir = options
            .zeroclaw_dir
            .clone()
            .unwrap_or_else(default_zeroclaw_dir);
        let auth = AuthService::new(&state_dir, options.secrets_encrypt);
        let responses_url = resolve_responses_url(options)?;

        Ok(Self {
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

    let mut parsed = reqwest::Url::parse(candidate)
        .map_err(|_| anyhow::anyhow!("OpenAI Codex endpoint override must be a valid URL"))?;

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

fn resolve_responses_url(options: &ProviderRuntimeOptions) -> anyhow::Result<String> {
    if let Some(endpoint) = std::env::var(CODEX_RESPONSES_URL_ENV)
        .ok()
        .and_then(|value| first_nonempty(Some(&value)))
    {
        return build_responses_url(&endpoint);
    }

    if let Some(base_url) = std::env::var(CODEX_BASE_URL_ENV)
        .ok()
        .and_then(|value| first_nonempty(Some(&value)))
    {
        return build_responses_url(&base_url);
    }

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

fn first_nonempty(text: Option<&str>) -> Option<String> {
    text.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

#[allow(dead_code)]
fn resolve_instructions(system_prompt: Option<&str>) -> String {
    first_nonempty(system_prompt).unwrap_or_else(|| DEFAULT_CODEX_INSTRUCTIONS.to_string())
}

fn normalize_model_id(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

fn convert_tools(tools: Option<&[ToolSpec]>) -> Option<Vec<ResponsesToolSpec>> {
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

fn build_responses_input(messages: &[ChatMessage]) -> (String, Vec<Value>) {
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
                    if let Some(call_id) = value.get("tool_call_id").and_then(Value::as_str) {
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
                        input.push(response_message_item(
                            "tool",
                            vec![serde_json::json!({
                                "type": "output_text",
                                "text": msg.content,
                            })],
                        ));
                    }
                } else if !msg.content.trim().is_empty() {
                    input.push(serde_json::json!({
                        "type": "function_call_output",
                        "call_id": uuid::Uuid::new_v4().to_string(),
                        "output": msg.content,
                    }));
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
        .map(ToString::to_string)
        .or_else(|| std::env::var("ZEROCLAW_CODEX_REASONING_EFFORT").ok())
        .and_then(|value| first_nonempty(Some(&value)))
        .unwrap_or_else(|| "xhigh".to_string())
        .to_ascii_lowercase();
    clamp_reasoning_effort(model_id, &raw)
}

fn nonempty_preserve(text: Option<&str>) -> Option<String> {
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
        for content in &item.content {
            if content.kind.as_deref() == Some("output_text")
                && let Some(text) = first_nonempty(content.text.as_deref())
            {
                return Some(text);
            }
        }
    }

    for item in &response.output {
        for content in &item.content {
            if let Some(text) = first_nonempty(content.text.as_deref()) {
                return Some(text);
            }
        }
    }

    None
}

fn extract_responses_tool_calls(response: &ResponsesResponse) -> Vec<ProviderToolCall> {
    response
        .output
        .iter()
        .filter(|item| item.kind.as_deref() == Some("function_call"))
        .filter_map(|item| {
            let name = item.name.clone()?;
            let arguments = item.arguments.clone().unwrap_or_default();
            Some(ProviderToolCall {
                id: item
                    .call_id
                    .clone()
                    .or_else(|| item.id.clone())
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                name,
                arguments,
                extra_content: None,
            })
        })
        .collect()
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

fn process_responses_stream_event(
    event: Value,
    state: &mut ResponsesStreamState,
) -> anyhow::Result<Vec<StreamEvent>> {
    if let Some(message) = extract_stream_error_message(&event) {
        anyhow::bail!("OpenAI Codex stream error: {message}");
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
                match item.get("type").and_then(Value::as_str) {
                    Some("message") if !state.saw_text_delta => {
                        if state.fallback_text.is_none() {
                            state.fallback_text = response_output_text_from_event_item(item);
                        }
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

fn process_sse_chunk(
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
        if let Ok(event) = serde_json::from_str::<Value>(line) {
            emitted.extend(process_responses_stream_event(event, state)?);
        }
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

fn extract_stream_error_message(event: &Value) -> Option<String> {
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

fn append_utf8_stream_chunk(
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
                return Err(anyhow::anyhow!(
                    "OpenAI Codex response contained invalid UTF-8: {err}"
                ));
            }

            // `error_len == None` means we have a valid prefix and an incomplete
            // multi-byte sequence at the end; keep it buffered until next chunk.
            Ok(())
        }
    }
}

#[allow(dead_code)]
fn decode_utf8_stream_chunks<'a, I>(chunks: I) -> anyhow::Result<String>
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let mut body = String::new();
    let mut pending = Vec::new();

    for chunk in chunks {
        append_utf8_stream_chunk(&mut body, &mut pending, chunk)?;
    }

    if !pending.is_empty() {
        let err = std::str::from_utf8(&pending).expect_err("pending bytes should be invalid UTF-8");
        return Err(anyhow::anyhow!(
            "OpenAI Codex response ended with incomplete UTF-8: {err}"
        ));
    }

    Ok(body)
}

fn parse_responses_body(body: &str) -> anyhow::Result<ResponsesTurnResult> {
    if body.contains("data:") || body.contains("event:") {
        let result = parse_sse_turn(body)?;
        return ensure_nonempty_responses_turn(result, || {
            anyhow::anyhow!(
                "No response from OpenAI Codex stream payload: {}",
                super::sanitize_api_error(body)
            )
        });
    }

    let body_trimmed = body.trim_start();
    let looks_like_sse = body_trimmed.starts_with("event:") || body_trimmed.starts_with("data:");
    if looks_like_sse {
        return Err(anyhow::anyhow!(
            "No response from OpenAI Codex stream payload: {}",
            super::sanitize_api_error(body)
        ));
    }

    let parsed: ResponsesResponse = serde_json::from_str(body).map_err(|err| {
        anyhow::anyhow!(
            "OpenAI Codex JSON parse failed: {err}. Payload: {}",
            super::sanitize_api_error(body)
        )
    })?;
    let result = ResponsesTurnResult {
        text: extract_responses_text(&parsed),
        tool_calls: extract_responses_tool_calls(&parsed),
    };
    ensure_nonempty_responses_turn(result, || {
        anyhow::anyhow!(
            "No response from OpenAI Codex: {}",
            super::sanitize_api_error(body)
        )
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
        let bytes = chunk
            .map_err(|err| anyhow::anyhow!("error reading OpenAI Codex response stream: {err}"))?;
        append_utf8_stream_chunk(&mut body, &mut pending_utf8, &bytes)?;
    }

    if !pending_utf8.is_empty() {
        let err = std::str::from_utf8(&pending_utf8)
            .expect_err("pending bytes should be invalid UTF-8 at end of stream");
        return Err(anyhow::anyhow!(
            "OpenAI Codex response ended with incomplete UTF-8: {err}"
        ));
    }

    parse_responses_body(&body)
}

impl OpenAiCodexProvider {
    async fn send_responses_request(
        &self,
        input: Vec<Value>,
        instructions: String,
        tools: Option<Vec<ResponsesToolSpec>>,
        model: &str,
    ) -> anyhow::Result<ResponsesTurnResult> {
        let use_gateway_api_key_auth = self.custom_endpoint && self.gateway_api_key.is_some();
        let profile = match self
            .auth
            .get_profile("openai-codex", self.auth_profile_override.as_deref())
            .await
        {
            Ok(profile) => profile,
            Err(err) if use_gateway_api_key_auth => {
                tracing::warn!(
                    error = %err,
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
                tracing::warn!(
                    error = %err,
                    "failed to refresh OpenAI token; continuing with custom endpoint API key mode"
                );
                None
            }
            Err(err) => return Err(err),
        };

        let account_id = profile.and_then(|profile| profile.account_id).or_else(|| {
            oauth_access_token
                .as_deref()
                .and_then(extract_account_id_from_jwt)
        });
        let access_token = if use_gateway_api_key_auth {
            oauth_access_token
        } else {
            Some(oauth_access_token.ok_or_else(|| {
                anyhow::anyhow!(
                    "OpenAI Codex auth profile not found. Run `zeroclaw auth login --provider openai-codex`."
                )
            })?)
        };
        let account_id = if use_gateway_api_key_auth {
            account_id
        } else {
            Some(account_id.ok_or_else(|| {
                anyhow::anyhow!(
                    "OpenAI Codex account id not found in auth profile/token. Run `zeroclaw auth login --provider openai-codex` again."
                )
            })?)
        };
        let normalized_model = normalize_model_id(model);

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
                    self.reasoning_effort.as_deref(),
                ),
                summary: "auto".to_string(),
            },
            include: vec!["reasoning.encrypted_content".to_string()],
            tools,
            tool_choice: has_tools.then(|| "auto".to_string()),
            parallel_tool_calls: has_tools.then_some(true),
        };

        let bearer_token = if use_gateway_api_key_auth {
            self.gateway_api_key.as_deref().unwrap_or_default()
        } else {
            access_token.as_deref().unwrap_or_default()
        };

        let mut request_builder = self
            .client
            .post(&self.responses_url)
            .header("Authorization", format!("Bearer {bearer_token}"))
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "pi")
            .header("accept", "text/event-stream")
            .header("Content-Type", "application/json");

        if let Some(account_id) = account_id.as_deref() {
            request_builder = request_builder.header("chatgpt-account-id", account_id);
        }

        if use_gateway_api_key_auth {
            if let Some(access_token) = access_token.as_deref() {
                request_builder = request_builder.header("x-openai-access-token", access_token);
            }
            if let Some(account_id) = account_id.as_deref() {
                request_builder = request_builder.header("x-openai-account-id", account_id);
            }
        }

        let response = request_builder.json(&request).send().await?;

        if !response.status().is_success() {
            return Err(super::api_error("OpenAI Codex", response).await);
        }

        decode_responses_body(response).await
    }
}

#[async_trait]
impl Provider for OpenAiCodexProvider {
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
            reasoning_content: None,
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
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(16);

        tokio::spawn(async move {
            let config = zeroclaw_config::schema::MultimodalConfig::default();
            let prepared =
                match crate::multimodal::prepare_messages_for_provider(&messages, &config).await {
                    Ok(prepared) => prepared,
                    Err(err) => {
                        let _ = tx.send(Err(StreamError::Provider(err.to_string()))).await;
                        return;
                    }
                };

            let (instructions, input) = build_responses_input(&prepared.messages);
            let result = provider
                .send_responses_request(
                    input,
                    instructions,
                    convert_tools(tools.as_deref()),
                    &model,
                )
                .await;

            match result {
                Ok(response) => {
                    for tool_call in response.tool_calls {
                        if tx.send(Ok(StreamEvent::ToolCall(tool_call))).await.is_err() {
                            return;
                        }
                    }

                    if let Some(text) = response.text.filter(|text| !text.is_empty()) {
                        let chunk = if count_tokens {
                            StreamChunk::delta(text).with_token_estimate()
                        } else {
                            StreamChunk::delta(text)
                        };
                        if tx.send(Ok(StreamEvent::TextDelta(chunk))).await.is_err() {
                            return;
                        }
                    }

                    let _ = tx.send(Ok(StreamEvent::Final)).await;
                }
                Err(err) => {
                    let _ = tx.send(Err(StreamError::Provider(err.to_string()))).await;
                }
            }
        });

        stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{EnvGuard, env_lock};

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
            output: vec![ResponsesOutput {
                kind: None,
                id: None,
                call_id: None,
                name: None,
                arguments: None,
                content: vec![ResponsesContent {
                    kind: Some("output_text".into()),
                    text: Some("nested".into()),
                }],
            }],
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
    fn resolve_responses_url_prefers_explicit_endpoint_env() {
        let _lock = env_lock();
        let _endpoint_guard = EnvGuard::set(
            CODEX_RESPONSES_URL_ENV,
            Some("https://env.example.com/v1/responses"),
        );
        let _base_guard = EnvGuard::set(CODEX_BASE_URL_ENV, Some("https://base.example.com/v1"));

        let options = ProviderRuntimeOptions::default();
        assert_eq!(
            resolve_responses_url(&options).unwrap(),
            "https://env.example.com/v1/responses"
        );
    }

    #[test]
    fn resolve_responses_url_uses_provider_api_url_override() {
        let _lock = env_lock();
        let _endpoint_guard = EnvGuard::set(CODEX_RESPONSES_URL_ENV, None);
        let _base_guard = EnvGuard::set(CODEX_BASE_URL_ENV, None);

        let options = ProviderRuntimeOptions {
            provider_api_url: Some("https://proxy.example.com/v1".to_string()),
            ..ProviderRuntimeOptions::default()
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
        let options = ProviderRuntimeOptions {
            provider_api_url: Some("https://api.tonsof.blue/v1".to_string()),
            ..ProviderRuntimeOptions::default()
        };

        let provider = OpenAiCodexProvider::new(&options, Some("test-key")).unwrap();
        assert!(provider.custom_endpoint);
        assert_eq!(provider.gateway_api_key.as_deref(), Some("test-key"));
    }

    #[test]
    fn resolve_instructions_uses_default_when_missing() {
        assert_eq!(
            resolve_instructions(None),
            DEFAULT_CODEX_INSTRUCTIONS.to_string()
        );
    }

    #[test]
    fn resolve_instructions_uses_default_when_blank() {
        assert_eq!(
            resolve_instructions(Some("   ")),
            DEFAULT_CODEX_INSTRUCTIONS.to_string()
        );
    }

    #[test]
    fn resolve_instructions_uses_system_prompt_when_present() {
        assert_eq!(
            resolve_instructions(Some("Be strict")),
            "Be strict".to_string()
        );
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
        let _lock = env_lock();
        let _guard = EnvGuard::set("ZEROCLAW_CODEX_REASONING_EFFORT", Some("low"));
        assert_eq!(
            resolve_reasoning_effort("gpt-5-codex", Some("high")),
            "high".to_string()
        );
    }

    #[test]
    fn resolve_reasoning_effort_uses_legacy_env_when_unconfigured() {
        let _lock = env_lock();
        let _guard = EnvGuard::set("ZEROCLAW_CODEX_REASONING_EFFORT", Some("minimal"));
        assert_eq!(
            resolve_reasoning_effort("gpt-5-codex", None),
            "low".to_string()
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
                .contains("No response from OpenAI Codex stream payload"),
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
    fn decode_utf8_stream_chunks_handles_multibyte_split_across_chunks() {
        let payload = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello 世\"}\n\ndata: [DONE]\n";
        let bytes = payload.as_bytes();
        let split_at = payload.find('世').unwrap() + 1;

        let decoded = decode_utf8_stream_chunks([&bytes[..split_at], &bytes[split_at..]]).unwrap();
        assert_eq!(decoded, payload);
        assert_eq!(
            parse_sse_turn(&decoded).unwrap().text.as_deref(),
            Some("Hello 世")
        );
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
        let options = ProviderRuntimeOptions {
            secrets_encrypt: false,
            ..Default::default()
        };
        let provider =
            OpenAiCodexProvider::new(&options, None).expect("provider should initialize");
        let caps = provider.capabilities();

        assert!(caps.native_tool_calling);
        assert!(caps.vision);
    }
}
