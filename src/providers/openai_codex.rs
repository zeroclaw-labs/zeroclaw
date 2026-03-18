use crate::auth::openai_oauth::extract_account_id_from_jwt;
use crate::auth::AuthService;
use crate::multimodal;
use crate::providers::traits::{
    ChatMessage, ChatRequest, ChatResponse, Provider, ProviderCapabilities, ToolCall,
};
use crate::providers::ProviderRuntimeOptions;
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

const DEFAULT_CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const CODEX_RESPONSES_URL_ENV: &str = "ZEROCLAW_CODEX_RESPONSES_URL";
const CODEX_BASE_URL_ENV: &str = "ZEROCLAW_CODEX_BASE_URL";
const DEFAULT_CODEX_INSTRUCTIONS: &str =
    "You are ZeroClaw, a concise and helpful coding assistant.";

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
    input: Vec<ResponsesInputItem>,
    instructions: String,
    store: bool,
    stream: bool,
    text: ResponsesTextOptions,
    reasoning: ResponsesReasoningOptions,
    include: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ResponsesTool>,
}

#[derive(Debug, Serialize)]
struct ResponsesTool {
    #[serde(rename = "type")]
    kind: String,
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct ResponsesInput {
    role: String,
    content: Vec<ResponsesInputContent>,
}

/// Items that can appear in the Responses API `input` array.
///
/// The Responses API accepts a heterogeneous array: regular messages alongside
/// function_call / function_call_output items for multi-turn tool history.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ResponsesInputItem {
    /// A regular user/assistant message.
    Message(ResponsesInput),
    /// A function call the model made previously (replayed for history).
    FunctionCall {
        #[serde(rename = "type")]
        kind: String,
        call_id: String,
        name: String,
        arguments: String,
    },
    /// The output of a function call execution.
    FunctionCallOutput {
        #[serde(rename = "type")]
        kind: String,
        call_id: String,
        output: String,
    },
}

#[derive(Debug, Serialize)]
struct ResponsesInputContent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_url: Option<String>,
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
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    content: Vec<ResponsesContent>,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponsesContent {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
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
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
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

fn resolve_instructions(system_prompt: Option<&str>) -> String {
    first_nonempty(system_prompt).unwrap_or_else(|| DEFAULT_CODEX_INSTRUCTIONS.to_string())
}

fn normalize_model_id(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

fn build_responses_input(messages: &[ChatMessage]) -> (String, Vec<ResponsesInputItem>) {
    let mut system_parts: Vec<&str> = Vec::new();
    let mut input: Vec<ResponsesInputItem> = Vec::new();

    for msg in messages {
        match msg.role.as_str() {
            "system" => system_parts.push(&msg.content),
            "user" => {
                let (cleaned_text, image_refs) = multimodal::parse_image_markers(&msg.content);

                let mut content_items = Vec::new();

                // Add text if present
                if !cleaned_text.trim().is_empty() {
                    content_items.push(ResponsesInputContent {
                        kind: "input_text".to_string(),
                        text: Some(cleaned_text),
                        image_url: None,
                    });
                }

                // Add images
                for image_ref in image_refs {
                    content_items.push(ResponsesInputContent {
                        kind: "input_image".to_string(),
                        text: None,
                        image_url: Some(image_ref),
                    });
                }

                // If no content at all, add empty text
                if content_items.is_empty() {
                    content_items.push(ResponsesInputContent {
                        kind: "input_text".to_string(),
                        text: Some(String::new()),
                        image_url: None,
                    });
                }

                input.push(ResponsesInputItem::Message(ResponsesInput {
                    role: "user".to_string(),
                    content: content_items,
                }));
            }
            "assistant" => {
                // Check if the content is a serialized tool-calls JSON payload
                // (from NativeToolDispatcher: {"content":..., "tool_calls":[...]})
                if let Ok(parsed) = serde_json::from_str::<Value>(&msg.content) {
                    if let Some(tool_calls) = parsed.get("tool_calls").and_then(|v| v.as_array()) {
                        // Extract text content if present
                        let text_content = parsed
                            .get("content")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();

                        // Emit a text message for the assistant content (if non-empty)
                        if !text_content.trim().is_empty() {
                            input.push(ResponsesInputItem::Message(ResponsesInput {
                                role: "assistant".to_string(),
                                content: vec![ResponsesInputContent {
                                    kind: "output_text".to_string(),
                                    text: Some(text_content),
                                    image_url: None,
                                }],
                            }));
                        }

                        // Emit FunctionCall items for each tool call
                        for tc in tool_calls {
                            let call_id = tc
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown")
                                .to_string();
                            let name = tc
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown")
                                .to_string();
                            let arguments = tc
                                .get("arguments")
                                .and_then(|v| v.as_str())
                                .unwrap_or("{}")
                                .to_string();

                            input.push(ResponsesInputItem::FunctionCall {
                                kind: "function_call".to_string(),
                                call_id,
                                name,
                                arguments,
                            });
                        }
                        continue;
                    }
                }

                // Plain assistant text message
                input.push(ResponsesInputItem::Message(ResponsesInput {
                    role: "assistant".to_string(),
                    content: vec![ResponsesInputContent {
                        kind: "output_text".to_string(),
                        text: Some(msg.content.clone()),
                        image_url: None,
                    }],
                }));
            }
            "tool" => {
                // Tool results are serialized as JSON with tool_call_id and content.
                // Format: {"tool_call_id":"xxx","content":"result text"}
                if let Ok(parsed) = serde_json::from_str::<Value>(&msg.content) {
                    let call_id = parsed
                        .get("tool_call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let output = parsed
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&msg.content)
                        .to_string();

                    input.push(ResponsesInputItem::FunctionCallOutput {
                        kind: "function_call_output".to_string(),
                        call_id,
                        output,
                    });
                } else {
                    // Fallback: treat as raw output with unknown call_id
                    input.push(ResponsesInputItem::FunctionCallOutput {
                        kind: "function_call_output".to_string(),
                        call_id: "unknown".to_string(),
                        output: msg.content.clone(),
                    });
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
        .unwrap_or_else(|| "medium".to_string())
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
            if content.kind.as_deref() == Some("output_text") {
                if let Some(text) = first_nonempty(content.text.as_deref()) {
                    return Some(text);
                }
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

/// Extract tool calls from a full Responses API response object.
fn extract_responses_tool_calls(response: &ResponsesResponse) -> Vec<ToolCall> {
    let mut tool_calls = Vec::new();
    for item in &response.output {
        if item.kind.as_deref() == Some("function_call") {
            if let (Some(call_id), Some(name), Some(arguments)) =
                (&item.call_id, &item.name, &item.arguments)
            {
                tool_calls.push(ToolCall {
                    id: call_id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                });
            }
        }
    }
    tool_calls
}

/// Accumulator for a single function call being built from SSE deltas.
#[derive(Debug, Default)]
struct FunctionCallAccumulator {
    call_id: Option<String>,
    name: Option<String>,
    arguments: String,
}

/// Result of parsing SSE events from the Responses API stream.
struct ParsedSseResponse {
    text: Option<String>,
    tool_calls: Vec<ToolCall>,
}

fn extract_stream_event_text(event: &Value, saw_delta: bool) -> Option<String> {
    let event_type = event.get("type").and_then(Value::as_str);
    match event_type {
        Some("response.output_text.delta") => {
            nonempty_preserve(event.get("delta").and_then(Value::as_str))
        }
        Some("response.output_text.done") if !saw_delta => {
            nonempty_preserve(event.get("text").and_then(Value::as_str))
        }
        // For text extraction from completed response, handled separately in parse_sse_response
        _ => None,
    }
}

fn parse_sse_response(body: &str) -> anyhow::Result<ParsedSseResponse> {
    let mut saw_delta = false;
    let mut delta_accumulator = String::new();
    let mut fallback_text = None;
    let mut buffer = body.to_string();

    // Function call accumulators keyed by output_index
    let mut fc_accumulators: HashMap<u64, FunctionCallAccumulator> = HashMap::new();
    let mut completed_tool_calls: Vec<ToolCall> = Vec::new();
    let mut fallback_tool_calls: Vec<ToolCall> = Vec::new();

    let mut process_event = |event: Value| -> anyhow::Result<()> {
        if let Some(message) = extract_stream_error_message(&event) {
            return Err(anyhow::anyhow!("OpenAI Codex stream error: {message}"));
        }

        let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");

        match event_type {
            "response.output_item.added" => {
                // A new output item was added; if it's a function_call, start accumulating
                if let Some(item) = event.get("item") {
                    if item.get("type").and_then(Value::as_str) == Some("function_call") {
                        let output_index = event
                            .get("output_index")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        let mut acc = FunctionCallAccumulator::default();
                        acc.call_id = item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .map(String::from);
                        acc.name = item.get("name").and_then(Value::as_str).map(String::from);
                        fc_accumulators.insert(output_index, acc);
                    }
                }
            }
            "response.function_call_arguments.delta" => {
                let output_index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    fc_accumulators
                        .entry(output_index)
                        .or_default()
                        .arguments
                        .push_str(delta);
                }
            }
            "response.function_call_arguments.done" => {
                let output_index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if let Some(acc) = fc_accumulators.remove(&output_index) {
                    // Use the done event's arguments if available, otherwise the accumulated
                    let arguments = event
                        .get("arguments")
                        .and_then(Value::as_str)
                        .map(String::from)
                        .unwrap_or(acc.arguments);
                    completed_tool_calls.push(ToolCall {
                        id: acc
                            .call_id
                            .unwrap_or_else(|| format!("call_{output_index}")),
                        name: acc.name.unwrap_or_else(|| "unknown".to_string()),
                        arguments,
                    });
                }
            }
            "response.output_text.delta" => {
                if let Some(text) = extract_stream_event_text(&event, saw_delta) {
                    saw_delta = true;
                    delta_accumulator.push_str(&text);
                }
            }
            "response.output_text.done" => {
                if !saw_delta {
                    if let Some(text) = extract_stream_event_text(&event, saw_delta) {
                        fallback_text = Some(text);
                    }
                }
            }
            "response.completed" | "response.done" => {
                // Fallback: extract from the full response object
                if let Some(resp_value) = event.get("response") {
                    if let Ok(resp) =
                        serde_json::from_value::<ResponsesResponse>(resp_value.clone())
                    {
                        if fallback_text.is_none() && !saw_delta {
                            fallback_text = extract_responses_text(&resp);
                        }
                        // Extract tool calls from the completed response as fallback
                        let tc = extract_responses_tool_calls(&resp);
                        if !tc.is_empty() {
                            fallback_tool_calls = tc;
                        }
                    }
                }
            }
            _ => {}
        }

        Ok(())
    };

    let mut process_chunk = |chunk: &str| -> anyhow::Result<()> {
        let data_lines: Vec<String> = chunk
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(|line| line.trim().to_string())
            .collect();
        if data_lines.is_empty() {
            return Ok(());
        }

        let joined = data_lines.join("\n");
        let trimmed = joined.trim();
        if trimmed.is_empty() || trimmed == "[DONE]" {
            return Ok(());
        }

        if let Ok(event) = serde_json::from_str::<Value>(trimmed) {
            return process_event(event);
        }

        for line in data_lines {
            let line = line.trim();
            if line.is_empty() || line == "[DONE]" {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<Value>(line) {
                process_event(event)?;
            }
        }

        Ok(())
    };

    loop {
        let Some(idx) = buffer.find("\n\n") else {
            break;
        };

        let chunk = buffer[..idx].to_string();
        buffer = buffer[idx + 2..].to_string();
        process_chunk(&chunk)?;
    }

    if !buffer.trim().is_empty() {
        process_chunk(&buffer)?;
    }

    let text = if saw_delta {
        nonempty_preserve(Some(&delta_accumulator))
    } else {
        fallback_text
    };

    // Use streamed tool calls if we got any, otherwise fall back to completed response
    let tool_calls = if completed_tool_calls.is_empty() {
        fallback_tool_calls
    } else {
        completed_tool_calls
    };

    Ok(ParsedSseResponse { text, tool_calls })
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
    if pending.is_empty() {
        if let Ok(text) = std::str::from_utf8(chunk) {
            body.push_str(text);
            return Ok(());
        }
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

/// Read the response body incrementally via `bytes_stream()` to avoid
/// buffering the entire SSE payload in memory.  The previous implementation
/// used `response.text().await?` which holds the HTTP connection open until
/// every byte has arrived — on high-latency links the long-lived connection
/// often drops mid-read, producing the "error decoding response body" failure
/// reported in #3544.
async fn decode_responses_body(response: reqwest::Response) -> anyhow::Result<ChatResponse> {
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

    let body_trimmed = body.trim_start();
    let looks_like_sse = body_trimmed.starts_with("event:") || body_trimmed.starts_with("data:");

    if looks_like_sse {
        let parsed = parse_sse_response(&body)?;
        if parsed.text.is_some() || !parsed.tool_calls.is_empty() {
            return Ok(ChatResponse {
                text: parsed.text,
                tool_calls: parsed.tool_calls,
                usage: None,
                reasoning_content: None,
            });
        }
        return Err(anyhow::anyhow!(
            "No response from OpenAI Codex stream payload: {}",
            super::sanitize_api_error(&body)
        ));
    }

    // Non-streaming JSON response
    let parsed: ResponsesResponse = serde_json::from_str(&body).map_err(|err| {
        anyhow::anyhow!(
            "OpenAI Codex JSON parse failed: {err}. Payload: {}",
            super::sanitize_api_error(&body)
        )
    })?;

    let text = extract_responses_text(&parsed);
    let tool_calls = extract_responses_tool_calls(&parsed);

    if text.is_none() && tool_calls.is_empty() {
        return Err(anyhow::anyhow!("No response from OpenAI Codex"));
    }

    Ok(ChatResponse {
        text,
        tool_calls,
        usage: None,
        reasoning_content: None,
    })
}

impl OpenAiCodexProvider {
    async fn send_responses_request(
        &self,
        input: Vec<ResponsesInputItem>,
        instructions: String,
        model: &str,
        tools: Vec<ResponsesTool>,
    ) -> anyhow::Result<ChatResponse> {
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

        let has_tools = !tools.is_empty();

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
            tool_choice: if has_tools {
                Some("auto".to_string())
            } else {
                None
            },
            parallel_tool_calls: if has_tools { Some(true) } else { None },
            tools,
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
        _temperature: f64,
    ) -> anyhow::Result<String> {
        // Build temporary messages array
        let mut messages = Vec::new();
        if let Some(sys) = system_prompt {
            messages.push(ChatMessage::system(sys));
        }
        messages.push(ChatMessage::user(message));

        // Normalize images: convert file paths to data URIs
        let config = crate::config::MultimodalConfig::default();
        let prepared = crate::multimodal::prepare_messages_for_provider(&messages, &config).await?;

        let (instructions, input) = build_responses_input(&prepared.messages);
        let response = self
            .send_responses_request(input, instructions, model, Vec::new())
            .await?;
        Ok(response.text.unwrap_or_default())
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        _temperature: f64,
    ) -> anyhow::Result<String> {
        // Normalize image markers: convert file paths to data URIs
        let config = crate::config::MultimodalConfig::default();
        let prepared = crate::multimodal::prepare_messages_for_provider(messages, &config).await?;

        let (instructions, input) = build_responses_input(&prepared.messages);
        let response = self
            .send_responses_request(input, instructions, model, Vec::new())
            .await?;
        Ok(response.text.unwrap_or_default())
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        _temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let config = crate::config::MultimodalConfig::default();
        let prepared =
            crate::multimodal::prepare_messages_for_provider(request.messages, &config).await?;

        let (instructions, input) = build_responses_input(&prepared.messages);

        let tools: Vec<ResponsesTool> = request
            .tools
            .unwrap_or(&[])
            .iter()
            .map(|t| ResponsesTool {
                kind: "function".to_string(),
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            })
            .collect();

        self.send_responses_request(input, instructions, model, tools)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let original = std::env::var(key).ok();
            match value {
                Some(next) => std::env::set_var(key, next),
                None => std::env::remove_var(key),
            }
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(original) = self.original.as_deref() {
                std::env::set_var(self.key, original);
            } else {
                std::env::remove_var(self.key);
            }
        }
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
            output: vec![ResponsesOutput {
                kind: Some("message".into()),
                content: vec![ResponsesContent {
                    kind: Some("output_text".into()),
                    text: Some("nested".into()),
                }],
                call_id: None,
                name: None,
                arguments: None,
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
        let _guard = EnvGuard::set("ZEROCLAW_CODEX_REASONING_EFFORT", Some("low"));
        assert_eq!(
            resolve_reasoning_effort("gpt-5-codex", Some("high")),
            "high".to_string()
        );
    }

    #[test]
    fn resolve_reasoning_effort_uses_legacy_env_when_unconfigured() {
        let _guard = EnvGuard::set("ZEROCLAW_CODEX_REASONING_EFFORT", Some("minimal"));
        assert_eq!(
            resolve_reasoning_effort("gpt-5-codex", None),
            "low".to_string()
        );
    }

    #[test]
    fn parse_sse_response_reads_output_text_delta() {
        let payload = r#"data: {"type":"response.created","response":{"id":"resp_123"}}

data: {"type":"response.output_text.delta","delta":"Hello"}
data: {"type":"response.output_text.delta","delta":" world"}
data: {"type":"response.completed","response":{"output_text":"Hello world"}}
data: [DONE]
"#;

        let result = parse_sse_response(payload).unwrap();
        assert_eq!(result.text.as_deref(), Some("Hello world"));
        assert!(result.tool_calls.is_empty());
    }

    #[test]
    fn parse_sse_response_falls_back_to_completed_response() {
        let payload = r#"data: {"type":"response.completed","response":{"output_text":"Done"}}
data: [DONE]
"#;

        let result = parse_sse_response(payload).unwrap();
        assert_eq!(result.text.as_deref(), Some("Done"));
        assert!(result.tool_calls.is_empty());
    }

    #[test]
    fn parse_sse_response_extracts_function_calls_from_stream() {
        let payload = concat!(
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_abc\",\"name\":\"shell\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"command\\\"\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\": \\\"ls\\\"}\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"output_index\":0,\"arguments\":\"{\\\"command\\\": \\\"ls\\\"}\"}\n\n",
            "data: [DONE]\n",
        );

        let result = parse_sse_response(payload).unwrap();
        assert!(result.text.is_none());
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].id, "call_abc");
        assert_eq!(result.tool_calls[0].name, "shell");
        assert_eq!(result.tool_calls[0].arguments, r#"{"command": "ls"}"#);
    }

    #[test]
    fn parse_sse_response_extracts_multiple_function_calls() {
        let payload = concat!(
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"file_read\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"output_index\":0,\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_2\",\"name\":\"file_read\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"output_index\":1,\"arguments\":\"{\\\"path\\\":\\\"b.txt\\\"}\"}\n\n",
            "data: [DONE]\n",
        );

        let result = parse_sse_response(payload).unwrap();
        assert_eq!(result.tool_calls.len(), 2);
        assert_eq!(result.tool_calls[0].name, "file_read");
        assert_eq!(result.tool_calls[0].id, "call_1");
        assert_eq!(result.tool_calls[1].name, "file_read");
        assert_eq!(result.tool_calls[1].id, "call_2");
    }

    #[test]
    fn parse_sse_response_extracts_tool_calls_from_completed_fallback() {
        let payload = concat!(
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[",
            "{\"type\":\"function_call\",\"call_id\":\"call_fb\",\"name\":\"shell\",\"arguments\":\"{\\\"command\\\":\\\"pwd\\\"}\",\"content\":[]}",
            "]}}\n\n",
            "data: [DONE]\n",
        );

        let result = parse_sse_response(payload).unwrap();
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].id, "call_fb");
        assert_eq!(result.tool_calls[0].name, "shell");
    }

    #[test]
    fn parse_sse_response_mixed_text_and_tool_calls() {
        let payload = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Let me check\"}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_m\",\"name\":\"shell\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"output_index\":1,\"arguments\":\"{\\\"command\\\":\\\"ls\\\"}\"}\n\n",
            "data: [DONE]\n",
        );

        let result = parse_sse_response(payload).unwrap();
        assert_eq!(result.text.as_deref(), Some("Let me check"));
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "shell");
    }

    #[test]
    fn decode_utf8_stream_chunks_handles_multibyte_split_across_chunks() {
        let payload =
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello 世\"}\n\ndata: [DONE]\n";
        let bytes = payload.as_bytes();
        let split_at = payload.find('世').unwrap() + 1;

        let decoded = decode_utf8_stream_chunks([&bytes[..split_at], &bytes[split_at..]]).unwrap();
        assert_eq!(decoded, payload);
        assert_eq!(
            parse_sse_response(&decoded).unwrap().text.as_deref(),
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
    fn build_responses_input_handles_tool_messages() {
        let messages = vec![
            ChatMessage {
                role: "user".into(),
                content: "List files".into(),
            },
            ChatMessage {
                role: "assistant".into(),
                content: r#"{"content":"Let me check","tool_calls":[{"id":"call_1","name":"shell","arguments":"{\"command\":\"ls\"}"}]}"#.into(),
            },
            ChatMessage {
                role: "tool".into(),
                content: r#"{"tool_call_id":"call_1","content":"file1.txt\nfile2.txt"}"#.into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Thanks".into(),
            },
        ];
        let (_, input) = build_responses_input(&messages);

        let json: Vec<Value> = input
            .iter()
            .map(|item| serde_json::to_value(item).unwrap())
            .collect();

        // user message
        assert_eq!(json[0]["role"], "user");

        // assistant text message
        assert_eq!(json[1]["role"], "assistant");
        assert_eq!(json[1]["content"][0]["text"], "Let me check");

        // function_call item
        assert_eq!(json[2]["type"], "function_call");
        assert_eq!(json[2]["call_id"], "call_1");
        assert_eq!(json[2]["name"], "shell");

        // function_call_output item
        assert_eq!(json[3]["type"], "function_call_output");
        assert_eq!(json[3]["call_id"], "call_1");
        assert_eq!(json[3]["output"], "file1.txt\nfile2.txt");

        // final user message
        assert_eq!(json[4]["role"], "user");
    }

    #[test]
    fn build_responses_input_handles_assistant_tool_calls_without_text() {
        let messages = vec![
            ChatMessage {
                role: "assistant".into(),
                content: r#"{"content":null,"tool_calls":[{"id":"call_x","name":"memory_recall","arguments":"{}"}]}"#.into(),
            },
            ChatMessage {
                role: "tool".into(),
                content: r#"{"tool_call_id":"call_x","content":"No memories found"}"#.into(),
            },
        ];
        let (_, input) = build_responses_input(&messages);

        let json: Vec<Value> = input
            .iter()
            .map(|item| serde_json::to_value(item).unwrap())
            .collect();

        // No text message emitted (content was null/empty)
        assert_eq!(json[0]["type"], "function_call");
        assert_eq!(json[0]["call_id"], "call_x");

        assert_eq!(json[1]["type"], "function_call_output");
        assert_eq!(json[1]["call_id"], "call_x");
    }

    #[test]
    fn build_responses_input_handles_image_markers() {
        let messages = vec![ChatMessage::user(
            "Describe this\n\n[IMAGE:data:image/png;base64,abc]",
        )];
        let (_, input) = build_responses_input(&messages);

        assert_eq!(input.len(), 1);

        let json = serde_json::to_value(&input[0]).unwrap();
        let content = json["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);

        // First content = text
        assert_eq!(content[0]["type"], "input_text");
        assert!(content[0]["text"]
            .as_str()
            .unwrap()
            .contains("Describe this"));

        // Second content = image
        assert_eq!(content[1]["type"], "input_image");
        assert_eq!(content[1]["image_url"], "data:image/png;base64,abc");
    }

    #[test]
    fn build_responses_input_preserves_text_only_messages() {
        let messages = vec![ChatMessage::user("Hello without images")];
        let (_, input) = build_responses_input(&messages);

        assert_eq!(input.len(), 1);

        let json = serde_json::to_value(&input[0]).unwrap();
        let content = json["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[0]["text"], "Hello without images");
    }

    #[test]
    fn build_responses_input_handles_multiple_images() {
        let messages = vec![ChatMessage::user(
            "Compare these: [IMAGE:data:image/png;base64,img1] and [IMAGE:data:image/jpeg;base64,img2]",
        )];
        let (_, input) = build_responses_input(&messages);

        assert_eq!(input.len(), 1);

        let json = serde_json::to_value(&input[0]).unwrap();
        let content = json["content"].as_array().unwrap();
        assert_eq!(content.len(), 3); // text + 2 images
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[1]["type"], "input_image");
        assert_eq!(content[2]["type"], "input_image");
    }

    #[test]
    fn capabilities_includes_native_tool_calling_and_vision() {
        let options = ProviderRuntimeOptions {
            provider_api_url: None,
            zeroclaw_dir: None,
            secrets_encrypt: false,
            auth_profile_override: None,
            reasoning_enabled: None,
            reasoning_effort: None,
            provider_timeout_secs: None,
            extra_headers: std::collections::HashMap::new(),
            api_path: None,
        };
        let provider =
            OpenAiCodexProvider::new(&options, None).expect("provider should initialize");
        let caps = provider.capabilities();

        assert!(caps.native_tool_calling);
        assert!(caps.vision);
    }

    #[test]
    fn responses_tool_serialization() {
        let tool = ResponsesTool {
            kind: "function".to_string(),
            name: "shell".to_string(),
            description: "Execute shell commands".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                }
            }),
        };

        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(json["type"], "function");
        assert_eq!(json["name"], "shell");
        assert_eq!(json["description"], "Execute shell commands");
        assert!(json["parameters"]["properties"]["command"].is_object());
    }

    #[test]
    fn responses_request_omits_tool_fields_when_empty() {
        let request = ResponsesRequest {
            model: "gpt-5-codex".to_string(),
            input: vec![],
            instructions: "test".to_string(),
            store: false,
            stream: true,
            text: ResponsesTextOptions {
                verbosity: "medium".to_string(),
            },
            reasoning: ResponsesReasoningOptions {
                effort: "medium".to_string(),
                summary: "auto".to_string(),
            },
            include: vec![],
            tool_choice: None,
            parallel_tool_calls: None,
            tools: vec![],
        };

        let json = serde_json::to_value(&request).unwrap();
        assert!(!json.as_object().unwrap().contains_key("tool_choice"));
        assert!(!json
            .as_object()
            .unwrap()
            .contains_key("parallel_tool_calls"));
        assert!(!json.as_object().unwrap().contains_key("tools"));
    }

    #[test]
    fn responses_request_includes_tool_fields_when_present() {
        let request = ResponsesRequest {
            model: "gpt-5-codex".to_string(),
            input: vec![],
            instructions: "test".to_string(),
            store: false,
            stream: true,
            text: ResponsesTextOptions {
                verbosity: "medium".to_string(),
            },
            reasoning: ResponsesReasoningOptions {
                effort: "medium".to_string(),
                summary: "auto".to_string(),
            },
            include: vec![],
            tool_choice: Some("auto".to_string()),
            parallel_tool_calls: Some(true),
            tools: vec![ResponsesTool {
                kind: "function".to_string(),
                name: "test".to_string(),
                description: "test tool".to_string(),
                parameters: serde_json::json!({}),
            }],
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["tool_choice"], "auto");
        assert_eq!(json["parallel_tool_calls"], true);
        assert_eq!(json["tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn extract_responses_tool_calls_from_response() {
        let response = ResponsesResponse {
            output: vec![
                ResponsesOutput {
                    kind: Some("message".into()),
                    content: vec![ResponsesContent {
                        kind: Some("output_text".into()),
                        text: Some("checking".into()),
                    }],
                    call_id: None,
                    name: None,
                    arguments: None,
                },
                ResponsesOutput {
                    kind: Some("function_call".into()),
                    content: vec![],
                    call_id: Some("call_99".into()),
                    name: Some("shell".into()),
                    arguments: Some(r#"{"command":"ls"}"#.into()),
                },
            ],
            output_text: Some("checking".into()),
        };

        let text = extract_responses_text(&response);
        assert_eq!(text.as_deref(), Some("checking"));

        let tool_calls = extract_responses_tool_calls(&response);
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "call_99");
        assert_eq!(tool_calls[0].name, "shell");
        assert_eq!(tool_calls[0].arguments, r#"{"command":"ls"}"#);
    }

    #[test]
    fn function_call_input_item_serialization() {
        let item = ResponsesInputItem::FunctionCall {
            kind: "function_call".to_string(),
            call_id: "call_1".to_string(),
            name: "shell".to_string(),
            arguments: r#"{"command":"ls"}"#.to_string(),
        };

        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["type"], "function_call");
        assert_eq!(json["call_id"], "call_1");
        assert_eq!(json["name"], "shell");
    }

    #[test]
    fn function_call_output_item_serialization() {
        let item = ResponsesInputItem::FunctionCallOutput {
            kind: "function_call_output".to_string(),
            call_id: "call_1".to_string(),
            output: "file1.txt".to_string(),
        };

        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["type"], "function_call_output");
        assert_eq!(json["call_id"], "call_1");
        assert_eq!(json["output"], "file1.txt");
    }

    #[test]
    fn build_responses_input_tool_message_without_json() {
        // Fallback: raw tool output that isn't JSON
        let messages = vec![ChatMessage {
            role: "tool".into(),
            content: "raw output text".into(),
        }];
        let (_, input) = build_responses_input(&messages);

        let json = serde_json::to_value(&input[0]).unwrap();
        assert_eq!(json["type"], "function_call_output");
        assert_eq!(json["call_id"], "unknown");
        assert_eq!(json["output"], "raw output text");
    }
}
