//! Restricted, explicitly bound ChatGPT plan usage over public Responses.
//! Text and client-side function tools. No API-key fallback or hosted tools.
use crate::auth::AuthService;
use crate::traits::{ChatMessage, ChatRequest, ChatResponse, ModelProvider, TokenUsage, ToolCall};
use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::time::Duration;
use zeroclaw_api::tool::ToolSpec;
use zeroclaw_config::schema::{ModelProviderConfig, WireApi};

const RESPONSES_URL: &str = "https://api.openai.com/v1/responses";
const MODELS_URL: &str = "https://api.openai.com/v1/models";

#[cfg(test)]
tokio::task_local! { static FIXTURE_REQUEST_TIMEOUT: Duration; }

fn request_timeout() -> Duration {
    #[cfg(test)]
    if let Ok(timeout) = FIXTURE_REQUEST_TIMEOUT.try_with(|timeout| *timeout) {
        return timeout;
    }
    Duration::from_secs(300)
}

fn protocol_error(message: &'static str) -> anyhow::Error {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(serde_json::json!({"reason":message})),
        "ChatGPT plan transport failure"
    );
    anyhow::Error::msg(message)
}

pub struct ChatGptPlanProvider {
    alias: String,
    registration: String,
    auth: AuthService,
    client: reqwest::Client,
    responses_url: String,
    models_url: String,
    native_tools: bool,
}

impl ChatGptPlanProvider {
    pub(crate) fn new(
        alias: &str,
        config: &ModelProviderConfig,
        key: Option<&str>,
        uri: Option<&str>,
        opts: &crate::ModelProviderRuntimeOptions,
    ) -> Result<Self> {
        anyhow::ensure!(
            cfg!(unix),
            "ChatGPT plan usage is unsupported on native Windows in this slice"
        );
        let binding = config
            .chatgpt_plan_auth
            .as_ref()
            .context("Explicit ChatGPT plan auth reference required")?;
        anyhow::ensure!(
            binding.registration.starts_with("chatgpt-plan:")
                && binding.registration.len() > "chatgpt-plan:".len(),
            "Invalid ChatGPT plan registration reference"
        );
        anyhow::ensure!(
            !config.requires_openai_auth
                && config.api_key.is_none()
                && key.is_none()
                && opts.auth_profile_override.is_none(),
            "ChatGPT plan auth cannot be combined with Codex, API-key or global-profile credentials"
        );
        anyhow::ensure!(
            config.kind.as_deref() == Some("chatgpt-plan")
                && opts
                    .provider_kind
                    .as_deref()
                    .is_none_or(|kind| kind == "chatgpt-plan"),
            "ChatGPT plan auth requires kind = chatgpt-plan"
        );
        for endpoint in [uri, config.uri.as_deref(), opts.provider_api_url.as_deref()]
            .into_iter()
            .flatten()
        {
            anyhow::ensure!(
                matches!(endpoint, "https://api.openai.com/v1" | RESPONSES_URL),
                "ChatGPT plan usage requires the public OpenAI v1 endpoint"
            );
        }
        anyhow::ensure!(
            config
                .wire_api
                .is_none_or(|wire| wire == WireApi::Responses)
                && opts
                    .wire_api
                    .as_deref()
                    .is_none_or(|wire| wire == "responses"),
            "ChatGPT plan usage requires Responses"
        );
        anyhow::ensure!(
            config.extra_headers.is_empty()
                && opts.extra_headers.is_empty()
                && config.tls_ca_cert_path.is_none()
                && opts.tls_ca_cert_path.is_none()
                && opts.api_path.is_none(),
            "ChatGPT plan transport overrides are unsupported"
        );
        anyhow::ensure!(
            config.temperature.is_none()
                && config.max_tokens.is_none()
                && opts.provider_max_tokens.is_none()
                && config.provider_extra.is_none()
                && opts.provider_extra.is_none()
                && opts.chat_template_kwargs.is_none()
                && !config.merge_system_into_user
                && !opts.merge_system_into_user,
            "ChatGPT plan preview request parameters are unsupported"
        );
        anyhow::ensure!(
            config.fallback.is_empty(),
            "ChatGPT plan provider fallbacks are unsupported; no metered fallback is enabled"
        );
        let root = opts
            .zeroclaw_dir
            .as_deref()
            .context("ChatGPT plan usage requires an explicit instance directory")?;
        let client = reqwest::Client::builder()
            .timeout(request_timeout())
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(30))
            .build()
            .context("Unable to create ChatGPT plan inference client")?;
        let (responses_url, models_url) = (RESPONSES_URL.to_string(), MODELS_URL.to_string());
        #[cfg(any(test, feature = "test-helpers"))]
        let (responses_url, models_url) = crate::plan_test_transport::ENDPOINTS
            .try_with(|endpoints| (endpoints.responses.clone(), endpoints.models.clone()))
            .unwrap_or((responses_url, models_url));
        Ok(Self {
            alias: alias.into(),
            registration: binding.registration.clone(),
            auth: AuthService::new(root, opts.secrets_encrypt),
            client,
            responses_url,
            models_url,
            native_tools: opts.native_tools.or(config.native_tools) != Some(false),
        })
    }

    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: Option<f64>,
    ) -> Result<ChatResponse> {
        anyhow::ensure!(
            self.native_tools || tools.is_empty(),
            "ChatGPT native function tools are disabled for this provider"
        );
        let credential = self
            .auth
            .resolve_chatgpt_plan_credential(&self.registration)
            .await?;
        let (body, history_call_ids) = request_body(
            messages,
            tools,
            model,
            temperature,
            &self.registration,
            &credential.registration_provenance,
        )?;
        let response = tokio::time::timeout(
            Duration::from_secs(30),
            self.client
                .post(&self.responses_url)
                .bearer_auth(&credential.access_token)
                .header("Accept", "text/event-stream")
                .json(&body)
                .send(),
        )
        .await
        .map_err(|_| protocol_error("ChatGPT plan response headers timed out"))?
        .map_err(|_| protocol_error("ChatGPT plan request failed"))?;
        anyhow::ensure!(
            response.status() == reqwest::StatusCode::OK,
            "ChatGPT plan inference rejected the request (HTTP {}); no metered fallback",
            response.status().as_u16()
        );
        anyhow::ensure!(
            response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("text/event-stream")),
            "ChatGPT plan response is not an event stream"
        );
        let mut stream = response.bytes_stream();
        let mut pending = Vec::new();
        let mut sse = PlanSse::default();
        while let Some(chunk) = tokio::time::timeout(Duration::from_secs(300), stream.next())
            .await
            .map_err(|_| protocol_error("ChatGPT plan stream interrupted"))?
        {
            let bytes = chunk.map_err(|_| protocol_error("ChatGPT plan stream interrupted"))?;
            pending.extend_from_slice(&bytes);
            anyhow::ensure!(
                pending.len() <= 8 * 1024 * 1024,
                "ChatGPT plan stream event exceeds size limit"
            );
            loop {
                let end = pending
                    .windows(2)
                    .position(|w| w == b"\n\n")
                    .map(|end| (end, 2))
                    .or_else(|| {
                        pending
                            .windows(4)
                            .position(|w| w == b"\r\n\r\n")
                            .map(|end| (end, 4))
                    });
                let Some((end, width)) = end else {
                    break;
                };
                let event = std::str::from_utf8(&pending[..end])
                    .map_err(|_| protocol_error("Invalid ChatGPT stream encoding"))?;
                sse.event(event)?;
                pending.drain(..end + width);
            }
            // Process every event already in the terminal read, including a
            // late failure. Do not wait for a server to close after completion.
            if sse.completed {
                anyhow::ensure!(
                    pending.iter().all(u8::is_ascii_whitespace),
                    "ChatGPT plan stream has an incomplete event after completion"
                );
                return sse.finish(
                    tools,
                    &self.registration,
                    &credential.registration_provenance,
                    &history_call_ids,
                );
            }
        }
        if !pending.is_empty() {
            anyhow::bail!("ChatGPT plan stream ended with an incomplete event");
        }
        sse.finish(
            tools,
            &self.registration,
            &credential.registration_provenance,
            &history_call_ids,
        )
    }
}

fn function_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn function_call(item: &Value) -> Result<ToolCall> {
    anyhow::ensure!(
        item["namespace"] == "zeroclaw",
        "Invalid ChatGPT function namespace"
    );
    anyhow::ensure!(
        item.get("status")
            .is_none_or(|status| status == "completed"),
        "Invalid ChatGPT function call status"
    );
    anyhow::ensure!(
        item.get("async").is_none_or(|value| value == false)
            && item
                .get("caller")
                .is_none_or(|caller| caller.is_null() || caller["type"] == "direct"),
        "ChatGPT plan supports only direct client-side function calls"
    );
    let id = item["call_id"]
        .as_str()
        .context("Invalid ChatGPT function call ID")?;
    anyhow::ensure!(!id.trim().is_empty(), "Invalid ChatGPT function call ID");
    let name = item["name"]
        .as_str()
        .context("Invalid ChatGPT function name")?;
    anyhow::ensure!(function_name(name), "Invalid ChatGPT function name");
    let arguments = item["arguments"]
        .as_str()
        .context("Invalid ChatGPT function arguments")?;
    anyhow::ensure!(
        serde_json::from_str::<Value>(arguments).is_ok_and(|value| value.is_object()),
        "Invalid ChatGPT function arguments"
    );
    Ok(ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: arguments.into(),
        extra_content: None,
    })
}

fn supported_output_item(item: &Value) -> Result<()> {
    anyhow::ensure!(
        matches!(
            item["type"].as_str(),
            Some("message" | "reasoning" | "function_call")
        ),
        "Unsupported ChatGPT plan output type"
    );
    Ok(())
}

fn request_body(
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    model: &str,
    temperature: Option<f64>,
    registration: &str,
    registration_provenance: &str,
) -> Result<(Value, HashSet<String>)> {
    anyhow::ensure!(
        temperature.is_none(),
        "ChatGPT plan preview does not support temperature"
    );
    anyhow::ensure!(!model.trim().is_empty(), "ChatGPT plan model is required");
    let mut input = Vec::new();
    let mut instructions = Vec::new();
    let mut pending_calls = HashSet::new();
    let mut seen_call_ids = HashSet::new();
    for (index, message) in messages.iter().enumerate() {
        if ChatMessage::should_skip_internal_pruning_marker(messages, index) {
            continue;
        }
        match message.role.as_str() {
            "system" | "developer" => instructions.push(message.content.clone()),
            "user" => input.push(json!({"role":"user","content":message.content})),
            "assistant" => {
                let envelope = serde_json::from_str::<Value>(&message.content).ok();
                if let Some(envelope) = envelope
                    .as_ref()
                    .filter(|value| value.get("tool_calls").is_some())
                {
                    let calls: Vec<ToolCall> =
                        serde_json::from_value(envelope["tool_calls"].clone())
                            .map_err(|_| protocol_error("Invalid ChatGPT tool-call history"))?;
                    for call in &calls {
                        anyhow::ensure!(
                            call.extra_content
                                .as_ref()
                                .is_some_and(
                                    |extra| extra["chatgpt_plan"]["registration_provenance"]
                                        == registration_provenance
                                ),
                            "ChatGPT tool-call history has missing or different registration provenance; start a fresh compatible history"
                        );
                    }
                    let replay = envelope
                        .get("reasoning_content")
                        .and_then(Value::as_str)
                        .map(|value| {
                            serde_json::from_str::<Value>(value)
                                .map_err(|_| protocol_error("Invalid ChatGPT reasoning history"))
                        })
                        .transpose()?;
                    let items = if let Some(replay) = replay {
                        anyhow::ensure!(
                            replay["provider"] == "chatgpt-plan"
                                && replay["registration"] == registration,
                            "ChatGPT reasoning history belongs to a different registration"
                        );
                        anyhow::ensure!(
                            replay["registration_provenance"] == registration_provenance,
                            "ChatGPT reasoning history belongs to a different registration provenance"
                        );
                        let items = replay["items"]
                            .as_array()
                            .context("Invalid ChatGPT reasoning history items")?
                            .clone();
                        let replay_calls = items
                            .iter()
                            .filter(|item| item["type"] == "function_call")
                            .map(function_call)
                            .collect::<Result<Vec<_>>>()?;
                        anyhow::ensure!(
                            calls.len() == replay_calls.len()
                                && calls
                                    .iter()
                                    .zip(&replay_calls)
                                    .all(|(call, replay)| call.id == replay.id
                                        && call.name == replay.name
                                        && call.arguments == replay.arguments),
                            "ChatGPT reasoning history does not match its tool calls"
                        );
                        items
                    } else {
                        calls.iter().map(|call| json!({"type":"function_call","namespace":"zeroclaw","call_id":call.id,"name":call.name,"arguments":call.arguments})).collect()
                    };
                    if !items.iter().any(|item| item["type"] == "message")
                        && let Some(text) =
                            envelope["content"].as_str().filter(|text| !text.is_empty())
                    {
                        input.push(json!({"role":"assistant","content":text}));
                    }
                    for item in items {
                        supported_output_item(&item)?;
                        if item["type"] == "function_call" {
                            let call = function_call(&item)?;
                            anyhow::ensure!(
                                seen_call_ids.insert(call.id.clone()),
                                "Duplicate ChatGPT tool-call history ID"
                            );
                            pending_calls.insert(call.id);
                        }
                        input.push(item);
                    }
                } else {
                    input.push(json!({"role":"assistant","content":message.content}));
                }
            }
            "tool" => {
                let result: Value = serde_json::from_str(&message.content)
                    .map_err(|_| protocol_error("Invalid ChatGPT tool-result history"))?;
                let id = result["tool_call_id"]
                    .as_str()
                    .context("ChatGPT tool result missing call ID")?;
                anyhow::ensure!(
                    pending_calls.remove(id),
                    "ChatGPT tool result has no matching call ID"
                );
                let output = result["content"]
                    .as_str()
                    .context("ChatGPT tool result missing text")?;
                input.push(json!({"type":"function_call_output","call_id":id,"output":output}));
            }
            _ => anyhow::bail!("Unsupported ChatGPT message role"),
        }
    }
    anyhow::ensure!(
        pending_calls.is_empty(),
        "ChatGPT tool-call history has missing results"
    );
    anyhow::ensure!(!input.is_empty(), "ChatGPT plan input is required");
    let mut body = json!({"model":model,"input":input,"store":false,"stream":true});
    if !instructions.is_empty() {
        body["instructions"] = instructions.join("\n\n").into();
    }
    if !tools.is_empty() {
        let mut names = HashSet::new();
        let functions = tools.iter().map(|tool| {
            anyhow::ensure!(function_name(&tool.name) && names.insert(&tool.name), "Invalid or duplicate ChatGPT function name");
            anyhow::ensure!(tool.parameters.is_object(), "ChatGPT function parameters must be a JSON schema object");
            Ok(json!({"type":"function","name":tool.name,"description":tool.description,"parameters":tool.parameters,"strict":false}))
        }).collect::<Result<Vec<_>>>()?;
        body["tools"] = json!([{"type":"namespace","name":"zeroclaw","tools":functions}]);
        body["include"] = json!(["reasoning.encrypted_content"]);
    }
    Ok((body, seen_call_ids))
}

fn parse_raw_function(value: &Value) -> Result<ToolSpec> {
    let object = value
        .as_object()
        .context("Only client-side function tool specifications are supported")?;
    anyhow::ensure!(
        value["type"] == "function",
        "Only client-side function tools are supported"
    );
    let function = if let Some(function) = object.get("function") {
        anyhow::ensure!(
            object
                .keys()
                .all(|key| matches!(key.as_str(), "type" | "function")),
            "Only client-side function tool fields are supported"
        );
        function
    } else {
        value
    };
    let fields = function
        .as_object()
        .context("Only client-side function tool fields are supported")?;
    anyhow::ensure!(
        fields.keys().all(|key| matches!(
            key.as_str(),
            "type" | "name" | "description" | "parameters" | "strict"
        )) && fields.get("strict").is_none_or(|value| value == false),
        "Only client-side function tool fields are supported"
    );
    let name = function["name"]
        .as_str()
        .context("Client-side function tool missing name")?;
    let description = function
        .get("description")
        .map(|value| {
            value
                .as_str()
                .context("Invalid client-side function description")
        })
        .transpose()?
        .unwrap_or("");
    let parameters = function
        .get("parameters")
        .context("Client-side function tool missing parameters")?;
    Ok(ToolSpec::new(name, description, parameters.clone()))
}

#[derive(Default)]
struct PlanSse {
    completed: bool,
    text: String,
    output: Vec<Value>,
    usage: Option<TokenUsage>,
}
impl PlanSse {
    fn event(&mut self, event: &str) -> Result<()> {
        let data = event
            .lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            return Ok(());
        }
        anyhow::ensure!(
            data != "[DONE]",
            "ChatGPT plan stream omitted response.completed"
        );
        let event: Value = serde_json::from_str(&data)
            .map_err(|_| protocol_error("Malformed ChatGPT plan stream event"))?;
        match event["type"]
            .as_str()
            .context("ChatGPT stream event missing type")?
        {
            "response.failed" | "response.incomplete" | "error" => {
                let code = event["response"]["error"]["code"]
                    .as_str()
                    .or_else(|| event["code"].as_str());
                match code {
                    Some("subscription_sharing_usage_limit_exceeded") => anyhow::bail!(
                        "ChatGPT plan usage limit reached; review ChatGPT usage settings"
                    ),
                    Some("subscription_sharing_usage_unavailable") => anyhow::bail!(
                        "ChatGPT plan usage unavailable; review ChatGPT app permissions"
                    ),
                    _ => anyhow::bail!("ChatGPT plan response failed or was incomplete"),
                }
            }
            "response.output_text.delta" => {
                anyhow::ensure!(!self.completed, "ChatGPT text arrived after completion");
                self.text.push_str(
                    event["delta"]
                        .as_str()
                        .context("ChatGPT stream delta missing text")?,
                );
                anyhow::ensure!(
                    self.text.len() <= 16 * 1024 * 1024,
                    "ChatGPT response exceeds size limit"
                );
            }
            "response.completed" => {
                anyhow::ensure!(
                    !self.completed && event["response"]["status"] == "completed",
                    "Invalid ChatGPT response completion"
                );
                if let Some(output) = event["response"]["output"].as_array() {
                    for item in output {
                        supported_output_item(item)?;
                    }
                    self.output = output.clone();
                }
                if self.text.is_empty() {
                    if let Some(text) = event["response"]["output_text"].as_str() {
                        self.text = text.into();
                    } else if let Some(output) = event["response"]["output"].as_array() {
                        for item in output {
                            if item["type"] == "message"
                                && let Some(parts) = item["content"].as_array()
                            {
                                for part in parts {
                                    if part["type"] == "output_text"
                                        && let Some(text) = part["text"].as_str()
                                    {
                                        self.text.push_str(text);
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(usage) = event["response"]
                    .get("usage")
                    .filter(|usage| !usage.is_null())
                {
                    self.usage = Some(TokenUsage {
                        input_tokens: usage["input_tokens"].as_u64(),
                        output_tokens: usage["output_tokens"].as_u64(),
                        cached_input_tokens: usage["input_tokens_details"]["cached_tokens"]
                            .as_u64(),
                        cache_creation_input_tokens: None,
                    });
                }
                self.completed = true;
            }
            "response.output_item.added" | "response.output_item.done" => {
                supported_output_item(&event["item"])?
            }
            _ => {}
        }
        Ok(())
    }
    fn finish(
        &self,
        tools: &[ToolSpec],
        registration: &str,
        registration_provenance: &str,
        history_call_ids: &HashSet<String>,
    ) -> Result<ChatResponse> {
        anyhow::ensure!(
            self.completed,
            "ChatGPT plan stream ended before response.completed"
        );
        let mut ids = HashSet::new();
        let tool_calls = self
            .output
            .iter()
            .filter(|item| item["type"] == "function_call")
            .map(|item| {
                let mut call = function_call(item)?;
                anyhow::ensure!(
                    tools.iter().any(|tool| tool.name == call.name),
                    "ChatGPT response called an unoffered tool"
                );
                anyhow::ensure!(
                    ids.insert(call.id.clone()),
                    "Duplicate ChatGPT function call ID"
                );
                anyhow::ensure!(
                    !history_call_ids.contains(&call.id),
                    "ChatGPT function call ID collides with supplied history"
                );
                // Native history and retained snapshots already preserve this
                // per-call metadata even when opaque reasoning is stripped.
                call.extra_content = Some(
                    json!({"chatgpt_plan":{"registration_provenance":registration_provenance}}),
                );
                Ok(call)
            })
            .collect::<Result<Vec<_>>>()?;
        anyhow::ensure!(
            !self.text.trim().is_empty() || !tool_calls.is_empty(),
            "ChatGPT plan response completed without text or tool calls"
        );
        let reasoning_content = if tool_calls.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&json!({"provider":"chatgpt-plan","registration":registration,"registration_provenance":registration_provenance,"items":self.output}))
                .context("Unable to retain ChatGPT function-call history")?)
        };
        Ok(ChatResponse {
            text: (!self.text.is_empty()).then(|| self.text.clone()),
            tool_calls,
            usage: self.usage.clone(),
            reasoning_content,
        })
    }
}

#[async_trait::async_trait]
impl ModelProvider for ChatGptPlanProvider {
    fn supports_native_tools(&self) -> bool {
        self.native_tools
    }
    fn default_base_url(&self) -> Option<&str> {
        Some(RESPONSES_URL)
    }
    fn default_wire_api(&self) -> &str {
        "responses"
    }
    async fn chat_with_system(
        &self,
        system: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> Result<String> {
        let mut messages = Vec::new();
        if let Some(system) = system {
            messages.push(ChatMessage::system(system));
        }
        messages.push(ChatMessage::user(message));
        self.complete(&messages, &[], model, temperature)
            .await?
            .text
            .context("ChatGPT plan response completed without text")
    }
    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
    ) -> Result<String> {
        self.complete(messages, &[], model, temperature)
            .await?
            .text
            .context("ChatGPT plan response completed without text")
    }
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> Result<ChatResponse> {
        self.complete(
            request.messages,
            request.tools.unwrap_or(&[]),
            model,
            temperature,
        )
        .await
    }
    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        model: &str,
        temperature: Option<f64>,
    ) -> Result<ChatResponse> {
        let tools = tools
            .iter()
            .map(parse_raw_function)
            .collect::<Result<Vec<_>>>()?;
        self.complete(messages, &tools, model, temperature).await
    }
    async fn list_models(&self) -> Result<Vec<String>> {
        let access = self
            .auth
            .get_valid_chatgpt_plan_access_token(&self.registration)
            .await?;
        let response = tokio::time::timeout(
            Duration::from_secs(30),
            self.client.get(&self.models_url).bearer_auth(access).send(),
        )
        .await
        .map_err(|_| protocol_error("ChatGPT model catalog timed out"))?
        .map_err(|_| protocol_error("ChatGPT model catalog request failed"))?;
        anyhow::ensure!(
            response.status() == reqwest::StatusCode::OK,
            "ChatGPT model catalog unavailable (HTTP {})",
            response.status().as_u16()
        );
        let bytes = tokio::time::timeout(
            Duration::from_secs(30),
            crate::compatible::read_body_capped(response, 1024 * 1024),
        )
        .await
        .map_err(|_| protocol_error("ChatGPT model catalog timed out"))??;
        let catalog: Value = serde_json::from_slice(&bytes)
            .map_err(|_| protocol_error("Invalid ChatGPT model catalog"))?;
        let models = catalog["models"]
            .as_array()
            .context("ChatGPT model catalog missing models")?;
        Ok(models
            .iter()
            .filter(|model| model["visibility"] == "list")
            .filter_map(|model| model["slug"].as_str().map(str::to_owned))
            .collect())
    }
}
impl zeroclaw_api::attribution::Attributable for ChatGptPlanProvider {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Provider(zeroclaw_api::attribution::ProviderKind::Model(
            zeroclaw_api::attribution::ModelProviderKind::OpenAi,
        ))
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[cfg(all(test, unix))]
#[path = "chatgpt_plan_tests.rs"]
mod tests;
