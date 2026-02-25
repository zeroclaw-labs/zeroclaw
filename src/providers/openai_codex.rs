use crate::auth::openai_oauth::extract_account_id_from_jwt;
use crate::auth::AuthService;
use crate::multimodal;
use crate::providers::traits::{ChatMessage, Provider, ProviderCapabilities};
use crate::providers::ProviderRuntimeOptions;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
    client: Client,
}

#[derive(Debug, Serialize)]
struct ResponsesRequest {
    model: String,
    input: Vec<ResponsesInput>,
    instructions: String,
    store: bool,
    stream: bool,
    text: ResponsesTextOptions,
    reasoning: ResponsesReasoningOptions,
    include: Vec<String>,
    tool_choice: String,
    parallel_tool_calls: bool,
}

#[derive(Debug, Serialize)]
struct ResponsesInput {
    role: String,
    content: Vec<ResponsesInputContent>,
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
    #[serde(default)]
    content: Vec<ResponsesContent>,
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

fn build_responses_input(messages: &[ChatMessage]) -> (String, Vec<ResponsesInput>) {
    let mut system_parts: Vec<&str> = Vec::new();
    let mut input: Vec<ResponsesInput> = Vec::new();

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

                input.push(ResponsesInput {
                    role: "user".to_string(),
                    content: content_items,
                });
            }
            "assistant" => {
                input.push(ResponsesInput {
                    role: "assistant".to_string(),
                    content: vec![ResponsesInputContent {
                        kind: "output_text".to_string(),
                        text: Some(msg.content.clone()),
                        image_url: None,
                    }],
                });
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
            "xhigh" => "high".to_string(),
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

fn resolve_reasoning_effort(model_id: &str) -> String {
    let raw = std::env::var("ZEROCLAW_CODEX_REASONING_EFFORT")
        .ok()
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

fn extract_stream_event_text(event: &Value, saw_delta: bool) -> Option<String> {
    let event_type = event.get("type").and_then(Value::as_str);
    match event_type {
        Some("response.output_text.delta") => {
            nonempty_preserve(event.get("delta").and_then(Value::as_str))
        }
        Some("response.output_text.done") if !saw_delta => {
            nonempty_preserve(event.get("text").and_then(Value::as_str))
        }
        Some("response.completed" | "response.done") => event
            .get("response")
            .and_then(|value| serde_json::from_value::<ResponsesResponse>(value.clone()).ok())
            .and_then(|response| extract_responses_text(&response)),
        _ => None,
    }
}

fn parse_sse_text(body: &str) -> anyhow::Result<Option<String>> {
    let mut saw_delta = false;
    let mut delta_accumulator = String::new();
    let mut fallback_text = None;
    let mut buffer = body.to_string();

    let mut process_event = |event: Value| -> anyhow::Result<()> {
        if let Some(message) = extract_stream_error_message(&event) {
            return Err(anyhow::anyhow!("OpenAI Codex stream error: {message}"));
        }
        if let Some(text) = extract_stream_event_text(&event, saw_delta) {
            let event_type = event.get("type").and_then(Value::as_str);
            if event_type == Some("response.output_text.delta") {
                saw_delta = true;
                delta_accumulator.push_str(&text);
            } else if fallback_text.is_none() {
                fallback_text = Some(text);
            }
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

    if saw_delta {
        return Ok(nonempty_preserve(Some(&delta_accumulator)));
    }

    Ok(fallback_text)
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

async fn decode_responses_body(response: reqwest::Response) -> anyhow::Result<String> {
    let body = response.text().await?;

    if let Some(text) = parse_sse_text(&body)? {
        return Ok(text);
    }

    let body_trimmed = body.trim_start();
    let looks_like_sse = body_trimmed.starts_with("event:") || body_trimmed.starts_with("data:");
    if looks_like_sse {
        return Err(anyhow::anyhow!(
            "No response from OpenAI Codex stream payload: {}",
            super::sanitize_api_error(&body)
        ));
    }

    let parsed: ResponsesResponse = serde_json::from_str(&body).map_err(|err| {
        anyhow::anyhow!(
            "OpenAI Codex JSON parse failed: {err}. Payload: {}",
            super::sanitize_api_error(&body)
        )
    })?;
    extract_responses_text(&parsed).ok_or_else(|| anyhow::anyhow!("No response from OpenAI Codex"))
}

impl OpenAiCodexProvider {
    async fn send_responses_request(
        &self,
        input: Vec<ResponsesInput>,
        instructions: String,
        model: &str,
    ) -> anyhow::Result<String> {
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
                effort: resolve_reasoning_effort(normalized_model),
                summary: "auto".to_string(),
            },
            include: vec!["reasoning.encrypted_content".to_string()],
            tool_choice: "auto".to_string(),
            parallel_tool_calls: true,
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
            native_tool_calling: false,
            vision: true,
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
        self.send_responses_request(input, instructions, model)
            .await
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
        self.send_responses_request(input, instructions, model)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

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

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock poisoned")
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
        let _env_lock = env_lock();
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
        let _env_lock = env_lock();
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
        let _env_lock = env_lock();
        let _endpoint_guard = EnvGuard::set(CODEX_RESPONSES_URL_ENV, None);
        let _base_guard = EnvGuard::set(CODEX_BASE_URL_ENV, None);

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
    fn parse_sse_text_reads_output_text_delta() {
        let payload = r#"data: {"type":"response.created","response":{"id":"resp_123"}}

data: {"type":"response.output_text.delta","delta":"Hello"}
data: {"type":"response.output_text.delta","delta":" world"}
data: {"type":"response.completed","response":{"output_text":"Hello world"}}
data: [DONE]
"#;

        assert_eq!(
            parse_sse_text(payload).unwrap().as_deref(),
            Some("Hello world")
        );
    }

    #[test]
    fn parse_sse_text_falls_back_to_completed_response() {
        let payload = r#"data: {"type":"response.completed","response":{"output_text":"Done"}}
data: [DONE]
"#;

        assert_eq!(parse_sse_text(payload).unwrap().as_deref(), Some("Done"));
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
    fn build_responses_input_ignores_unknown_roles() {
        let messages = vec![
            ChatMessage {
                role: "tool".into(),
                content: "result".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Go".into(),
            },
        ];
        let (instructions, input) = build_responses_input(&messages);
        assert_eq!(instructions, DEFAULT_CODEX_INSTRUCTIONS);
        assert_eq!(input.len(), 1);
        let json = serde_json::to_value(&input[0]).unwrap();
        assert_eq!(json["role"], "user");
    }

    #[test]
    fn build_responses_input_handles_image_markers() {
        let messages = vec![ChatMessage::user(
            "Describe this\n\n[IMAGE:data:image/png;base64,abc]",
        )];
        let (_, input) = build_responses_input(&messages);

        assert_eq!(input.len(), 1);
        assert_eq!(input[0].role, "user");
        assert_eq!(input[0].content.len(), 2);

        let json: Vec<Value> = input[0]
            .content
            .iter()
            .map(|item| serde_json::to_value(item).unwrap())
            .collect();

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
        assert_eq!(input[0].content.len(), 1);

        let json = serde_json::to_value(&input[0].content[0]).unwrap();
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
        assert_eq!(input[0].content.len(), 3); // text + 2 images

        let json: Vec<Value> = input[0]
            .content
            .iter()
            .map(|item| serde_json::to_value(item).unwrap())
            .collect();

        assert_eq!(json[0]["type"], "input_text");
        assert_eq!(json[1]["type"], "input_image");
        assert_eq!(json[2]["type"], "input_image");
    }

    #[test]
    fn capabilities_includes_vision() {
        let options = ProviderRuntimeOptions {
            provider_api_url: None,
            zeroclaw_dir: None,
            secrets_encrypt: false,
            auth_profile_override: None,
            reasoning_enabled: None,
            custom_provider_api_mode: None,
            max_tokens_override: None,
            model_support_vision: None,
        };
        let provider =
            OpenAiCodexProvider::new(&options, None).expect("provider should initialize");
        let caps = provider.capabilities();

        assert!(!caps.native_tool_calling);
        assert!(caps.vision);
    }
}
