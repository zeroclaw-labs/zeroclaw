//! llama.cpp provider — always routes through the OpenResponses `/v1/responses` API.
//!
//! llama.cpp's responses endpoint is the only path that supports streaming tool
//! events correctly for local models; the chat-completions path is not used.

use crate::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    Provider, StreamChunk, StreamError, StreamEvent, StreamOptions, StreamResult,
    ToolCall as ProviderToolCall,
};
use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use reqwest::{
    Client,
    header::{HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, warn};

// ── Request / response structs ──────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ResponsesRequest {
    model: String,
    input: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enable_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    // Passes enable_thinking into the Jinja chat template — the top-level
    // enable_thinking field is not read by llama.cpp's responses endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_template_kwargs: Option<serde_json::Value>,
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
    #[serde(default, alias = "id")]
    call_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponsesContent {
    text: Option<String>,
}

// ── Provider struct ─────────────────────────────────────────────────────────

pub struct LlamaCppProvider {
    base_url: String,
    credential: Option<String>,
    /// `None` → let the model decide; `Some(false)` → disable thinking.
    think: Option<bool>,
    timeout_secs: u64,
    extra_headers: HashMap<String, String>,
    max_tokens: Option<u32>,
    /// Passed verbatim as `chat_template_kwargs` in the request body.
    /// Users set model-specific template variables here (e.g. `{"enable_thinking": false}`
    /// for Qwen3, or whatever the template expects for other model families).
    chat_template_kwargs: Option<serde_json::Value>,
}

impl LlamaCppProvider {
    pub fn new(base_url: &str, credential: Option<&str>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            credential: credential.map(str::to_string),
            think: None,
            timeout_secs: 120,
            extra_headers: HashMap::new(),
            max_tokens: None,
            chat_template_kwargs: None,
        }
    }

    pub fn with_think(mut self, think: Option<bool>) -> Self {
        self.think = think;
        self
    }

    pub fn with_timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: Option<u32>) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn with_extra_headers(mut self, headers: HashMap<String, String>) -> Self {
        self.extra_headers = headers;
        self
    }

    pub fn with_chat_template_kwargs(mut self, kwargs: Option<serde_json::Value>) -> Self {
        self.chat_template_kwargs = kwargs;
        self
    }

    // ── Internal helpers ────────────────────────────────────────────────────

    fn responses_url(&self) -> String {
        let base = &self.base_url;
        // Already pointing at the responses endpoint.
        if base.ends_with("/responses") {
            return base.clone();
        }
        // Derive sibling /responses from /chat/completions.
        if let Some(prefix) = base.strip_suffix("/chat/completions") {
            return format!("{prefix}/responses");
        }
        // If the base is a bare host (http://host or http://host:port with no
        // path), add the standard /v1 prefix that llama-server uses.
        let after_scheme = base.split_once("://").map(|(_, r)| r).unwrap_or(base);
        if !after_scheme.contains('/') {
            return format!("{base}/v1/responses");
        }
        format!("{base}/responses")
    }

    fn default_temperature(&self) -> f64 {
        0.4
    }

    fn auth_header(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.credential {
            Some(key) if !key.is_empty() => req.header("Authorization", format!("Bearer {key}")),
            _ => req,
        }
    }

    /// HTTP client with total timeout — for non-streaming requests only.
    fn http_client(&self) -> Client {
        let timeout = self.timeout_secs;
        if self.extra_headers.is_empty() {
            return zeroclaw_config::schema::build_runtime_proxy_client_with_timeouts(
                "provider.llamacpp",
                timeout,
                10,
            );
        }
        let mut headers = HeaderMap::new();
        for (key, value) in &self.extra_headers {
            match (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                (Ok(name), Ok(val)) => {
                    headers.insert(name, val);
                }
                _ => {
                    warn!(header = key, "Skipping invalid extra header");
                }
            }
        }
        let builder = Client::builder()
            .timeout(std::time::Duration::from_secs(timeout))
            .connect_timeout(std::time::Duration::from_secs(10))
            .default_headers(headers);
        let builder =
            zeroclaw_config::schema::apply_runtime_proxy_to_builder(builder, "provider.llamacpp");
        builder.build().unwrap_or_else(|e| {
            warn!("Failed to build llama.cpp HTTP client: {e}");
            Client::new()
        })
    }

    /// HTTP client with connect timeout only — for streaming SSE sessions.
    fn streaming_http_client(&self) -> Client {
        if self.extra_headers.is_empty() {
            let builder = Client::builder().connect_timeout(std::time::Duration::from_secs(10));
            let builder = zeroclaw_config::schema::apply_runtime_proxy_to_builder(
                builder,
                "provider.llamacpp",
            );
            return builder.build().unwrap_or_else(|e| {
                warn!("Failed to build llama.cpp streaming client: {e}");
                Client::new()
            });
        }
        let mut headers = HeaderMap::new();
        for (key, value) in &self.extra_headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                headers.insert(name, val);
            }
        }
        let builder = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .default_headers(headers);
        let builder =
            zeroclaw_config::schema::apply_runtime_proxy_to_builder(builder, "provider.llamacpp");
        builder.build().unwrap_or_else(|e| {
            warn!("Failed to build llama.cpp streaming client: {e}");
            Client::new()
        })
    }

    fn convert_tools(
        tools: Option<&[zeroclaw_api::tool::ToolSpec]>,
    ) -> Option<Vec<serde_json::Value>> {
        tools.map(|items| {
            items
                .iter()
                .map(|tool| {
                    let params = zeroclaw_api::schema::SchemaCleanr::clean_for_openai(
                        tool.parameters.clone(),
                    );
                    serde_json::json!({
                        "type": "function",
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": params,
                    })
                })
                .collect()
        })
    }

    // ── Core request methods ────────────────────────────────────────────────

    async fn do_chat(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
        tools: Option<Vec<serde_json::Value>>,
    ) -> anyhow::Result<ProviderChatResponse> {
        let (raw_instructions, input) = build_prompt(messages);
        if input.is_empty() {
            anyhow::bail!("llama.cpp: at least one non-system message is required");
        }
        let instructions = if tools.as_ref().is_some_and(|t| !t.is_empty()) {
            strip_tools_section(raw_instructions)
        } else {
            raw_instructions
        };
        let request = ResponsesRequest {
            model: model.to_string(),
            input,
            instructions,
            stream: Some(false),
            temperature: Some(temperature.unwrap_or(self.default_temperature())),
            tools,
            enable_thinking: self.think,
            max_output_tokens: self.max_tokens,
            chat_template_kwargs: self.chat_template_kwargs.clone(),
        };
        let url = self.responses_url();
        let response = self
            .auth_header(self.http_client().post(&url).json(&request))
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(super::api_error("llama.cpp", response).await);
        }
        let body = response.text().await?;
        parse_response_body(&body)
    }

    fn do_stream(
        &self,
        messages: Vec<ChatMessage>,
        model: &str,
        temperature: Option<f64>,
        tools: Option<Vec<serde_json::Value>>,
        count_tokens: bool,
    ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
        let (raw_instructions, input) = build_prompt(&messages);
        if input.is_empty() {
            return stream::once(async {
                Err(StreamError::Provider(
                    "llama.cpp: at least one non-system message is required".into(),
                ))
            })
            .boxed();
        }
        let instructions = if tools.as_ref().is_some_and(|t| !t.is_empty()) {
            strip_tools_section(raw_instructions)
        } else {
            raw_instructions
        };
        let req_body = ResponsesRequest {
            model: model.to_string(),
            input,
            instructions,
            stream: Some(true),
            temperature: Some(temperature.unwrap_or(self.default_temperature())),
            tools,
            enable_thinking: self.think,
            max_output_tokens: self.max_tokens,
            chat_template_kwargs: self.chat_template_kwargs.clone(),
        };
        let payload = match serde_json::to_value(req_body) {
            Ok(p) => p,
            Err(e) => return stream::once(async move { Err(StreamError::Json(e)) }).boxed(),
        };
        let payload_bytes = payload.to_string().len();
        let instructions_bytes = payload
            .get("instructions")
            .and_then(|v| v.as_str())
            .map(str::len)
            .unwrap_or(0);
        let tools_bytes = payload
            .get("tools")
            .map(|v| v.to_string().len())
            .unwrap_or(0);
        let input_bytes = payload
            .get("input")
            .map(|v| v.to_string().len())
            .unwrap_or(0);
        debug!(
            "llama.cpp stream request payload size={payload_bytes} bytes \
             (instructions={instructions_bytes}, tools={tools_bytes}, input={input_bytes})"
        );
        let url = self.responses_url();
        let client = self.streaming_http_client();
        let credential = self.credential.clone();

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(100);
        tokio::spawn(async move {
            let mut req = client.post(&url).json(&payload);
            if let Some(key) = credential.as_deref().filter(|k| !k.is_empty()) {
                req = req.header("Authorization", format!("Bearer {key}"));
            }
            req = req.header("Accept", "text/event-stream");

            let response = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Err(StreamError::Http(e.to_string()))).await;
                    return;
                }
            };
            if !response.status().is_success() {
                let status = response.status();
                let error = response
                    .text()
                    .await
                    .unwrap_or_else(|_| format!("HTTP {status}"));
                let sanitized = super::sanitize_api_error(&error);
                let _ = tx
                    .send(Err(StreamError::Provider(format!("{status}: {sanitized}"))))
                    .await;
                return;
            }
            let mut events = parse_sse_responses(response, count_tokens);
            while let Some(event) = events.next().await {
                if tx.send(event).await.is_err() {
                    break;
                }
            }
        });
        stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|e| (e, rx)) }).boxed()
    }
}

// ── Provider trait impl ─────────────────────────────────────────────────────

#[async_trait]
impl Provider for LlamaCppProvider {
    fn capabilities(&self) -> zeroclaw_api::provider::ProviderCapabilities {
        zeroclaw_api::provider::ProviderCapabilities {
            native_tool_calling: true,
            vision: false,
            prompt_caching: false,
        }
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        anyhow::bail!("llama.cpp does not support model listing")
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let mut msgs = Vec::new();
        if let Some(sys) = system_prompt {
            msgs.push(ChatMessage::system(sys));
        }
        msgs.push(ChatMessage::user(message));
        let resp = self.do_chat(&msgs, model, temperature, None).await?;
        Ok(resp.text.unwrap_or_default())
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let resp = self.do_chat(messages, model, temperature, None).await?;
        Ok(resp.text.unwrap_or_default())
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ProviderChatResponse> {
        let converted: Vec<serde_json::Value> = tools
            .iter()
            .filter_map(|t| {
                let func = t.get("function")?;
                let name = func.get("name")?.as_str()?;
                let description = func
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("");
                let params = func
                    .get("parameters")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                Some(serde_json::json!({
                    "type": "function",
                    "name": name,
                    "description": description,
                    "parameters": params,
                }))
            })
            .collect();
        let tools_opt = if converted.is_empty() {
            None
        } else {
            Some(converted)
        };
        self.do_chat(messages, model, temperature, tools_opt).await
    }

    async fn chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ProviderChatResponse> {
        let tools = Self::convert_tools(request.tools);
        self.do_chat(request.messages, model, temperature, tools)
            .await
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
        let messages = request.messages.to_vec();
        let tools = Self::convert_tools(request.tools);
        self.do_stream(messages, model, temperature, tools, options.count_tokens)
    }

    fn stream_chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
        let mut msgs = Vec::new();
        if let Some(sys) = system_prompt {
            msgs.push(ChatMessage::system(sys));
        }
        msgs.push(ChatMessage::user(message));
        text_chunks(self.do_stream(msgs, model, temperature, None, options.count_tokens))
    }

    fn stream_chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
        text_chunks(self.do_stream(
            messages.to_vec(),
            model,
            temperature,
            None,
            options.count_tokens,
        ))
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        let url = self.responses_url();
        let _ = self
            .auth_header(self.http_client().get(&url))
            .send()
            .await?;
        Ok(())
    }
}

// ── Prompt builder ──────────────────────────────────────────────────────────

/// Remove the `## Tools` section from the system prompt when tools are already
/// sent as structured data in the request body. The section contains full JSON
/// schemas for every tool, which can be hundreds of KB and is redundant when
/// the model receives the same information via the `tools` field.
fn strip_tools_section(instructions: Option<String>) -> Option<String> {
    let s = instructions?;
    // Match "## Tools\n" at start or after a newline.
    let needle = "## Tools\n";
    let (prefix, rest) = if let Some(rest) = s.strip_prefix(needle) {
        ("", rest)
    } else if let Some(pos) = s.find(&format!("\n{needle}")) {
        (&s[..pos], &s[pos + 1 + needle.len()..])
    } else {
        return Some(s);
    };
    // Find the next top-level section header or end of string.
    let suffix = if let Some(next) = rest.find("\n## ") {
        &rest[next + 1..]
    } else {
        ""
    };
    let result = format!("{prefix}\n\n{suffix}").trim().to_string();
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

fn build_prompt(messages: &[ChatMessage]) -> (Option<String>, Vec<serde_json::Value>) {
    let mut sys_parts: Vec<String> = Vec::new();
    let mut input: Vec<serde_json::Value> = Vec::new();

    for message in messages {
        if message.content.trim().is_empty() {
            continue;
        }
        if message.role == "system" {
            sys_parts.push(message.content.clone());
            continue;
        }

        let item: serde_json::Value = match message.role.as_str() {
            "assistant" => {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content)
                    && let Some(calls) = value.get("tool_calls").and_then(|v| v.as_array())
                    && !calls.is_empty()
                {
                    let text = value
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if !text.is_empty() {
                        input.push(serde_json::json!({
                            "role": "assistant",
                            "type": "message",
                            "content": [{"type": "output_text", "text": text}]
                        }));
                    }
                    for call in calls {
                        let call_id = call
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = call
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let arguments = call
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}")
                            .to_string();
                        input.push(serde_json::json!({
                            "type": "function_call",
                            "call_id": call_id,
                            "name": name,
                            "arguments": arguments,
                        }));
                    }
                    continue;
                }
                serde_json::json!({
                    "role": "assistant",
                    "type": "message",
                    "content": [{"type": "output_text", "text": message.content}]
                })
            }
            "tool" => {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content) {
                    let call_id = value
                        .get("tool_call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let output = value
                        .get("content")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| message.content.clone());
                    serde_json::json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": output,
                    })
                } else {
                    serde_json::json!({
                        "role": "assistant",
                        "type": "message",
                        "content": [{"type": "output_text", "text": message.content}]
                    })
                }
            }
            _ => serde_json::json!({"role": "user", "content": message.content}),
        };
        input.push(item);
    }

    let instructions = if sys_parts.is_empty() {
        None
    } else {
        Some(sys_parts.join("\n\n"))
    };
    (instructions, input)
}

// ── Response parser ─────────────────────────────────────────────────────────

fn parse_response_body(body: &str) -> anyhow::Result<ProviderChatResponse> {
    debug!("llama.cpp response body: {body}");
    let resp = serde_json::from_str::<ResponsesResponse>(body).map_err(|e| {
        let snippet = super::sanitize_api_error(body);
        anyhow::anyhow!("llama.cpp responses API returned unexpected payload: {e}; body={snippet}")
    })?;

    let mut tool_calls: Vec<ProviderToolCall> = Vec::new();
    let mut text: Option<String> = resp.output_text.as_deref().and_then(nonempty);

    for item in resp.output {
        match item.kind.as_deref() {
            Some("function_call") => {
                if let Some(name) = item.name {
                    let arguments = item.arguments.unwrap_or_else(|| "{}".to_string());
                    let id = item
                        .call_id
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                    tool_calls.push(ProviderToolCall {
                        id,
                        name,
                        arguments,
                        extra_content: None,
                    });
                }
            }
            // Skip chain-of-thought reasoning items; the answer is in the message item.
            Some("reasoning") => {}
            _ => {
                if text.is_none() {
                    for content in &item.content {
                        if let Some(t) = content.text.as_deref().and_then(nonempty) {
                            text = Some(t);
                            break;
                        }
                    }
                }
            }
        }
    }

    Ok(ProviderChatResponse {
        text,
        tool_calls,
        usage: None,
        reasoning_content: None,
    })
}

fn nonempty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

// ── SSE stream parser ───────────────────────────────────────────────────────

fn parse_sse_responses(
    response: reqwest::Response,
    count_tokens: bool,
) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
    use crate::traits::StreamChunk;

    let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(100);

    tokio::spawn(async move {
        if let Err(e) = response.error_for_status_ref() {
            let _ = tx.send(Err(StreamError::Http(e.to_string()))).await;
            return;
        }

        let mut pending: HashMap<String, (String, String)> = HashMap::new();
        let mut buffer = String::new();
        let mut utf8_buf: Vec<u8> = Vec::new();
        let mut bytes_stream = response.bytes_stream();

        'outer: while let Some(item) = bytes_stream.next().await {
            let bytes = match item {
                Ok(b) => b,
                Err(e) => {
                    let _ = tx.send(Err(StreamError::Http(e.to_string()))).await;
                    return;
                }
            };

            utf8_buf.extend_from_slice(&bytes);
            let text = match std::str::from_utf8(&utf8_buf) {
                Ok(s) => {
                    let owned = s.to_string();
                    utf8_buf.clear();
                    owned
                }
                Err(e) => {
                    let valid_up_to = e.valid_up_to();
                    if valid_up_to == 0 && utf8_buf.len() < 4 {
                        continue;
                    }
                    let valid = String::from_utf8_lossy(&utf8_buf[..valid_up_to]).into_owned();
                    utf8_buf.drain(..valid_up_to);
                    valid
                }
            };
            if text.is_empty() {
                continue;
            }

            buffer.push_str(&text);

            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].to_string();
                buffer.drain(..=pos);
                let line = line.trim();
                if line.is_empty() || line.starts_with("event:") || line.starts_with(':') {
                    continue;
                }
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    break 'outer;
                }

                let event: serde_json::Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = tx.send(Err(StreamError::Json(e))).await;
                        return;
                    }
                };

                let kind = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
                debug!("llama.cpp SSE event type={kind:?}");

                match kind {
                    "response.output_text.delta" => {
                        match event.get("delta").and_then(|v| v.as_str()) {
                            Some(delta) if !delta.is_empty() => {
                                let mut chunk = StreamChunk::delta(delta.to_string());
                                if count_tokens {
                                    chunk = chunk.with_token_estimate();
                                }
                                if tx.send(Ok(StreamEvent::TextDelta(chunk))).await.is_err() {
                                    return;
                                }
                            }
                            _ => debug!("llama.cpp output_text.delta had no string delta: {event}"),
                        }
                    }
                    // Chain-of-thought reasoning content — discard, wait for output_text.delta.
                    "response.reasoning_text.delta"
                    | "response.reasoning_summary_text.delta"
                    | "response.reasoning.delta" => {}
                    "response.output_item.added" => {
                        if let Some(item) = event.get("item")
                            && item.get("type").and_then(|v| v.as_str()) == Some("function_call")
                        {
                            let call_id = item
                                .get("call_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = item
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            pending.insert(call_id, (name, String::new()));
                        }
                    }
                    "response.function_call_arguments.delta" => {
                        if let (Some(call_id), Some(delta)) = (
                            event.get("call_id").and_then(|v| v.as_str()),
                            event.get("delta").and_then(|v| v.as_str()),
                        ) && let Some((_, args)) = pending.get_mut(call_id)
                        {
                            args.push_str(delta);
                        }
                    }
                    "response.output_item.done" => {
                        if let Some(item) = event.get("item")
                            && item.get("type").and_then(|v| v.as_str()) == Some("function_call")
                        {
                            let call_id =
                                item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
                            if let Some((name, accumulated_args)) = pending.remove(call_id) {
                                let arguments = item
                                    .get("arguments")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string)
                                    .unwrap_or(accumulated_args);
                                let arguments = if arguments.trim().is_empty() {
                                    "{}".to_string()
                                } else {
                                    arguments
                                };
                                let tool_call = ProviderToolCall {
                                    id: call_id.to_string(),
                                    name,
                                    arguments,
                                    extra_content: None,
                                };
                                if tx.send(Ok(StreamEvent::ToolCall(tool_call))).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    "response.completed" => break 'outer,
                    _ => debug!("llama.cpp unhandled SSE event type={kind:?} full={event}"),
                }
            }
        }

        // Flush any tool calls whose done event was missed.
        for (call_id, (name, arguments)) in pending.drain() {
            let arguments = if arguments.trim().is_empty() {
                "{}".to_string()
            } else {
                arguments
            };
            let _ = tx
                .send(Ok(StreamEvent::ToolCall(ProviderToolCall {
                    id: call_id,
                    name,
                    arguments,
                    extra_content: None,
                })))
                .await;
        }

        let _ = tx.send(Ok(StreamEvent::Final)).await;
    });

    stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|e| (e, rx)) }).boxed()
}

/// Convert a `StreamEvent` stream into a `StreamChunk` stream (text only).
fn text_chunks(
    events: stream::BoxStream<'static, StreamResult<StreamEvent>>,
) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
    use crate::traits::StreamChunk;
    let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamChunk>>(100);
    tokio::spawn(async move {
        let mut events = events;
        while let Some(event) = events.next().await {
            match event {
                Ok(StreamEvent::TextDelta(chunk)) => {
                    if tx.send(Ok(chunk)).await.is_err() {
                        return;
                    }
                }
                Ok(StreamEvent::Final) => {
                    let _ = tx.send(Ok(StreamChunk::final_chunk())).await;
                    return;
                }
                Ok(_) => {}
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            }
        }
        let _ = tx.send(Ok(StreamChunk::final_chunk())).await;
    });
    stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|c| (c, rx)) }).boxed()
}

#[cfg(test)]
mod strip_tests {
    use super::strip_tools_section;

    #[test]
    fn strips_middle_section() {
        let input =
            "## Identity\n\nFoo.\n\n## Tools\n\n- **shell**: run things\n\n## Safety\n\nBar.";
        let result = strip_tools_section(Some(input.to_string())).unwrap();
        assert!(!result.contains("## Tools"), "Tools section should be gone");
        assert!(
            result.contains("## Identity"),
            "Identity section should remain"
        );
        assert!(result.contains("## Safety"), "Safety section should remain");
    }

    #[test]
    fn strips_leading_section() {
        let input = "## Tools\n\n- **shell**: run things\n\n## Safety\n\nBar.";
        let result = strip_tools_section(Some(input.to_string())).unwrap();
        assert!(!result.contains("## Tools"));
        assert!(result.contains("## Safety"));
    }

    #[test]
    fn strips_trailing_section() {
        let input = "## Identity\n\nFoo.\n\n## Tools\n\n- **shell**: run things";
        let result = strip_tools_section(Some(input.to_string())).unwrap();
        assert!(!result.contains("## Tools"));
        assert!(result.contains("## Identity"));
    }

    #[test]
    fn passthrough_when_no_tools_section() {
        let input = "## Identity\n\nFoo.\n\n## Safety\n\nBar.";
        let result = strip_tools_section(Some(input.to_string())).unwrap();
        assert_eq!(result, input.trim());
    }

    #[test]
    fn none_in_none_out() {
        assert!(strip_tools_section(None).is_none());
    }
}

#[cfg(test)]
mod url_tests {
    use super::LlamaCppProvider;

    fn provider(base: &str) -> LlamaCppProvider {
        LlamaCppProvider::new(base, None)
    }

    #[test]
    fn bare_host_gets_v1_prefix() {
        assert_eq!(
            provider("http://localhost:8080").responses_url(),
            "http://localhost:8080/v1/responses"
        );
    }

    #[test]
    fn v1_path_appends_responses() {
        assert_eq!(
            provider("http://localhost:8080/v1").responses_url(),
            "http://localhost:8080/v1/responses"
        );
    }

    #[test]
    fn chat_completions_derives_sibling() {
        assert_eq!(
            provider("http://localhost:8080/v1/chat/completions").responses_url(),
            "http://localhost:8080/v1/responses"
        );
    }

    #[test]
    fn explicit_responses_url_passthrough() {
        assert_eq!(
            provider("http://localhost:8080/v1/responses").responses_url(),
            "http://localhost:8080/v1/responses"
        );
    }

    #[test]
    fn custom_path_appends_responses() {
        assert_eq!(
            provider("http://localhost:8080/openai/v1").responses_url(),
            "http://localhost:8080/openai/v1/responses"
        );
    }
}

#[cfg(test)]
mod error_sanitization_tests {
    use super::parse_response_body;

    #[test]
    fn parse_error_redacts_api_key_shaped_values() {
        // Non-JSON body containing an OpenAI-style key — the key must not
        // appear verbatim in the user-visible error.
        let body = "upstream error: invalid api key sk-abc123def456ghi789 rejected";
        let err = parse_response_body(body).unwrap_err().to_string();
        assert!(
            !err.contains("sk-abc123"),
            "raw key must not appear in error: {err}"
        );
        assert!(
            err.contains("[REDACTED]"),
            "key should be replaced with [REDACTED]: {err}"
        );
    }

    #[test]
    fn parse_error_includes_sanitized_snippet() {
        // Non-secret bodies should still surface a truncated snippet.
        let body = "upstream returned plain text instead of JSON";
        let err = parse_response_body(body).unwrap_err().to_string();
        assert!(
            err.contains("plain text"),
            "sanitized snippet should appear in error: {err}"
        );
    }
}
