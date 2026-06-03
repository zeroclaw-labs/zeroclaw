//! Generic OpenAI-compatible model_provider.
//! Most LLM APIs follow the same `/v1/chat/completions` format.
//! This module provides a single implementation that works for all of them.

use crate::multimodal;
use crate::stream_guard::AbortOnDrop;
use crate::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    ModelProvider, StreamChunk, StreamError, StreamEvent, StreamOptions, StreamResult, TokenUsage,
    ToolCall as ProviderToolCall,
};
use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use reqwest::{
    Client,
    header::{HeaderMap, HeaderValue, USER_AGENT},
};
use serde::{Deserialize, Serialize};

/// A model_provider that speaks the OpenAI-compatible chat completions API.
/// Used by: Venice, Vercel AI Gateway, Cloudflare AI Gateway, Moonshot,
/// Synthetic, `OpenCode` Zen, `OpenCode` Go, `Z.AI`, `GLM`, `MiniMax`, Bedrock, Qianfan, Groq, Mistral, `xAI`, etc.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone)]
pub struct OpenAiCompatibleModelProvider {
    /// `[providers.models.<alias>]` key this provider was constructed
    /// under. Used by the `Attributable` impl so log emissions carry the
    /// real composite (`<type>.<alias>`) instead of the bare type.
    pub alias: String,
    pub name: String,
    pub base_url: String,
    pub credential: Option<String>,
    pub auth_header: AuthStyle,
    supports_vision: bool,
    user_agent: Option<String>,
    /// When true, collect all `system` messages and prepend their content
    /// to the first `user` message, then drop the system messages.
    /// Required for model_providers that reject `role: system` (e.g. MiniMax).
    merge_system_into_user: bool,
    /// Whether this model_provider supports OpenAI-style native tool calling.
    /// When false, tools are injected into the system prompt as text.
    native_tool_calling: bool,
    /// HTTP request timeout in seconds for LLM API calls. Default: 120.
    timeout_secs: u64,
    /// Extra HTTP headers to include in all API requests.
    extra_headers: std::collections::HashMap<String, String>,
    /// Optional reasoning effort for GPT-5/Codex-compatible backends.
    reasoning_effort: Option<String>,
    /// Custom API path suffix (e.g. "/v2/generate").
    /// When set, overrides the default `/chat/completions` path detection.
    api_path: Option<String>,
    /// Maximum output tokens to include in API requests.
    max_tokens: Option<u32>,
    /// models.dev catalog key for this model_provider (e.g. "xai").
    /// When set, `list_models` fetches from the models.dev catalog.
    models_dev_key: Option<String>,
    /// OpenRouter vendor prefix for this model_provider (e.g. "x-ai", "tencent").
    /// When set and the models.dev fallback returns no list, `list_models`
    /// filters OpenRouter's `/api/v1/models` for entries under this prefix
    /// and returns the slug list. Last-resort catalog source for providers
    /// that aren't in models.dev.
    openrouter_vendor_prefix: Option<String>,
    /// Apply the conservative tool-schema sanitizer when the served model
    /// is one whose runtime rejects standard OpenAI-style tool schemas
    /// (today: gemma-4 family on llama.cpp, where the empty-properties /
    /// non-string `default` quirks crash the tool-call parser). The check
    /// runs at tool conversion time against the runtime model id.
    local_model_tool_sanitize: bool,
    /// Some OpenAI-compatible local servers, such as Ollama, expose `/models`
    /// without authentication. Keep the default credential-gated for hosted
    /// providers so missing credentials still fall through to catalog sources.
    unauthenticated_model_listing: bool,
}

/// How the model_provider expects the API key to be sent.
#[derive(Debug, Clone)]
pub enum AuthStyle {
    /// `Authorization: Bearer <key>`
    Bearer,
    /// `x-api-key: <key>` (used by some Chinese model_providers)
    XApiKey,
    /// Custom header name
    Custom(String),
    /// Zhipu/GLM JWT auth: the credential is `id.secret`, and a short-lived
    /// JWT (HMAC-SHA256, 3.5 min expiry) is generated per request.
    /// Used by Z.AI and GLM model_providers.
    ZhipuJwt,
}

/// Generate a Zhipu JWT from an `id.secret` API key.
/// Returns `Authorization: Bearer <jwt>` value. Token is valid for 3.5 minutes.
fn zhipu_jwt_bearer(credential: &str) -> Result<String, String> {
    let (id, secret) = credential
        .split_once('.')
        .ok_or_else(|| "Zhipu API key must be in 'id.secret' format".to_string())?;

    #[allow(clippy::cast_possible_truncation)] // millis won't exceed u64 until year 584 million
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis() as u64;
    let exp_ms = now_ms + 210_000; // 3.5 minutes

    // Header: {"alg":"HS256","typ":"JWT","sign_type":"SIGN"}
    let header_b64 = base64url_no_pad(br#"{"alg":"HS256","typ":"JWT","sign_type":"SIGN"}"#);
    let payload = format!(r#"{{"api_key":"{id}","exp":{exp_ms},"timestamp":{now_ms}}}"#);
    let payload_b64 = base64url_no_pad(payload.as_bytes());

    let signing_input = format!("{header_b64}.{payload_b64}");
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
    let sig = ring::hmac::sign(&key, signing_input.as_bytes());
    let sig_b64 = base64url_no_pad(sig.as_ref());

    Ok(format!("Bearer {signing_input}.{sig_b64}"))
}

fn base64url_no_pad(data: &[u8]) -> String {
    use base64::engine::{Engine, general_purpose::URL_SAFE_NO_PAD};
    URL_SAFE_NO_PAD.encode(data)
}

/// Apply auth to a request builder (usable from spawned tasks without `&self`).
///
/// When `credential` is `None` (e.g. local LLM servers that require no API key),
/// the request is returned unchanged -- no auth header is added.
fn apply_auth_to_request(
    req: reqwest::RequestBuilder,
    style: &AuthStyle,
    credential: Option<&str>,
) -> reqwest::RequestBuilder {
    let credential = match credential {
        Some(c) => c,
        None => return req,
    };
    match style {
        AuthStyle::Bearer => req.header("Authorization", format!("Bearer {credential}")),
        AuthStyle::XApiKey => req.header("x-api-key", credential),
        AuthStyle::Custom(header) => req.header(header, credential),
        AuthStyle::ZhipuJwt => match zhipu_jwt_bearer(credential) {
            Ok(val) => req.header("Authorization", val),
            Err(_) => req.header("Authorization", format!("Bearer {credential}")),
        },
    }
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
    /// Pricing data from the provider's `/models` endpoint.
    /// Kilo Gateway: `{"pricing": {"prompt": "0", "completion": "0"}}`
    /// OpenRouter: `{"pricing": {"prompt": "0.000003", "completion": "0.000015"}}`
    /// Values are per-token rates (e.g. "0.000005" = $5/1M tokens).
    #[serde(default)]
    pricing: Option<zeroclaw_api::model_provider::ModelPricing>,
}

fn normalize_model_ids(body: ModelsResponse) -> Vec<String> {
    let mut ids: Vec<String> = body
        .data
        .into_iter()
        .map(|e| e.id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect();
    ids.sort();
    ids
}

/// Extract model IDs with pricing from a ModelsResponse.
/// Returns sorted list of `ModelInfo` with pricing data where available.
fn normalize_models_with_pricing(
    body: ModelsResponse,
) -> Vec<zeroclaw_api::model_provider::ModelInfo> {
    use zeroclaw_api::model_provider::ModelInfo;
    let mut models: Vec<ModelInfo> = body
        .data
        .into_iter()
        .filter(|e| !e.id.trim().is_empty())
        .map(|e| ModelInfo {
            id: e.id.trim().to_string(),
            pricing: e.pricing,
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

impl OpenAiCompatibleModelProvider {
    pub fn new(
        alias: &str,
        name: &str,
        base_url: &str,
        credential: Option<&str>,
        auth_style: AuthStyle,
    ) -> Self {
        Self::new_with_options(
            alias, name, base_url, credential, auth_style, false, None, false,
        )
    }

    pub fn new_with_vision(
        alias: &str,
        name: &str,
        base_url: &str,
        credential: Option<&str>,
        auth_style: AuthStyle,
        supports_vision: bool,
    ) -> Self {
        Self::new_with_options(
            alias,
            name,
            base_url,
            credential,
            auth_style,
            supports_vision,
            None,
            false,
        )
    }

    /// Create a model_provider with a custom User-Agent header.
    ///
    /// Some model_providers (for example Kimi Code) require a specific User-Agent
    /// for request routing and policy enforcement.
    pub fn new_with_user_agent(
        alias: &str,
        name: &str,
        base_url: &str,
        credential: Option<&str>,
        auth_style: AuthStyle,
        user_agent: &str,
    ) -> Self {
        Self::new_with_options(
            alias,
            name,
            base_url,
            credential,
            auth_style,
            false,
            Some(user_agent),
            false,
        )
    }

    pub fn new_with_user_agent_and_vision(
        alias: &str,
        name: &str,
        base_url: &str,
        credential: Option<&str>,
        auth_style: AuthStyle,
        user_agent: &str,
        supports_vision: bool,
    ) -> Self {
        Self::new_with_options(
            alias,
            name,
            base_url,
            credential,
            auth_style,
            supports_vision,
            Some(user_agent),
            false,
        )
    }

    /// For model_providers that do not support `role: system` (e.g. MiniMax).
    /// System prompt content is prepended to the first user message instead.
    pub fn new_merge_system_into_user(
        alias: &str,
        name: &str,
        base_url: &str,
        credential: Option<&str>,
        auth_style: AuthStyle,
    ) -> Self {
        Self::new_with_options(
            alias, name, base_url, credential, auth_style, false, None, true,
        )
    }

    fn new_with_options(
        alias: &str,
        name: &str,
        base_url: &str,
        credential: Option<&str>,
        auth_style: AuthStyle,
        supports_vision: bool,
        user_agent: Option<&str>,
        merge_system_into_user: bool,
    ) -> Self {
        Self {
            alias: alias.to_string(),
            name: name.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            credential: credential.map(ToString::to_string),
            auth_header: auth_style,
            supports_vision,
            user_agent: user_agent.map(ToString::to_string),
            merge_system_into_user,
            native_tool_calling: !merge_system_into_user,
            timeout_secs: 120,
            extra_headers: std::collections::HashMap::new(),
            reasoning_effort: None,
            api_path: None,
            max_tokens: None,
            models_dev_key: None,
            openrouter_vendor_prefix: None,
            local_model_tool_sanitize: false,
            unauthenticated_model_listing: false,
        }
    }
    /// Opt this provider into per-model conservative tool-schema sanitization.
    /// Today the only trigger is the gemma-4 family on llama.cpp, where the
    /// upstream tool-call parser rejects empty-properties / non-string
    /// `default` values. The check runs at convert-time against the runtime
    /// model id (not against the family) so the same provider instance
    /// happily serves llama, qwen, etc. without sanitization.
    pub fn with_local_model_tool_sanitize(mut self) -> Self {
        self.local_model_tool_sanitize = true;
        self
    }

    pub fn with_unauthenticated_model_listing(mut self) -> Self {
        self.unauthenticated_model_listing = true;
        self
    }

    /// Disable native tool calling, forcing prompt-guided tool use instead.
    pub fn without_native_tools(mut self) -> Self {
        self.native_tool_calling = false;
        self
    }

    /// Merge all system messages into the first user message before sending.
    /// Unlike `new_merge_system_into_user`, this preserves native tool calling.
    pub fn with_merge_system_into_user(mut self) -> Self {
        self.merge_system_into_user = true;
        self
    }

    /// Override the HTTP request timeout for LLM API calls.
    pub fn with_timeout_secs(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }

    /// Set extra HTTP headers to include in all API requests.
    pub fn with_extra_headers(
        mut self,
        headers: std::collections::HashMap<String, String>,
    ) -> Self {
        self.extra_headers = headers;
        self
    }

    /// Set reasoning effort for GPT-5/Codex-compatible chat-completions APIs.
    pub fn with_reasoning_effort(mut self, reasoning_effort: Option<String>) -> Self {
        self.reasoning_effort = reasoning_effort;
        self
    }

    /// Set a custom API path suffix for this model_provider.
    /// When set, replaces the default `/chat/completions` path.
    pub fn with_api_path(mut self, api_path: Option<String>) -> Self {
        self.api_path = api_path;
        self
    }

    /// Set the maximum output tokens for API requests.
    pub fn with_max_tokens(mut self, max_tokens: Option<u32>) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Set the models.dev catalog key for this model_provider.
    /// When set, `list_models` returns the catalog's model list for that key.
    pub fn with_models_dev_key(mut self, key: &str) -> Self {
        self.models_dev_key = Some(key.to_string());
        self
    }

    /// Set the OpenRouter vendor prefix for this model_provider (e.g. `"x-ai"`,
    /// `"tencent"`, `"rekaai"`). `list_models` falls back to this catalog when
    /// neither a credential nor a working `models.dev` entry is available.
    pub fn with_openrouter_vendor_prefix(mut self, prefix: &str) -> Self {
        self.openrouter_vendor_prefix = Some(prefix.to_string());
        self
    }

    /// Collect all `system` role messages and keep them in a provider-safe
    /// shape. Strict OpenAI-compatible endpoints accept a leading system
    /// message but reject system messages later in the history.
    fn flatten_system_messages(messages: &[ChatMessage], merge: bool) -> Vec<ChatMessage> {
        let mut saw_system = false;
        let mut system_content = String::new();
        let mut result: Vec<ChatMessage> = Vec::with_capacity(messages.len());

        for message in messages {
            if message.role == "system" {
                saw_system = true;
                if !message.content.is_empty() {
                    if !system_content.is_empty() {
                        system_content.push_str("\n\n");
                    }
                    system_content.push_str(&message.content);
                }
            } else {
                result.push(message.clone());
            }
        }

        if !saw_system {
            return messages.to_vec();
        }

        if system_content.is_empty() {
            return result;
        }

        if !merge {
            result.insert(0, ChatMessage::system(system_content));
            return result;
        }

        if let Some(first_user) = result.iter_mut().find(|m| m.role == "user") {
            if !system_content.is_empty() {
                first_user.content = format!("{system_content}\n\n{}", first_user.content);
            }
        } else {
            // No user message found: insert a synthetic user message with system content
            result.insert(0, ChatMessage::user(&system_content));
        }

        result
    }

    fn http_client(&self) -> Client {
        let timeout = self.timeout_secs;
        let has_user_agent = self.user_agent.is_some();
        let has_extra_headers = !self.extra_headers.is_empty();

        if has_user_agent || has_extra_headers {
            let mut headers = HeaderMap::new();
            if let Some(ua) = self.user_agent.as_deref()
                && let Ok(value) = HeaderValue::from_str(ua)
            {
                headers.insert(USER_AGENT, value);
            }
            for (key, value) in &self.extra_headers {
                match (
                    reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                    HeaderValue::from_str(value),
                ) {
                    (Ok(name), Ok(val)) => {
                        headers.insert(name, val);
                    }
                    _ => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"header": key})),
                            "Skipping invalid extra header name or value"
                        );
                    }
                }
            }

            let builder = Client::builder()
                .timeout(std::time::Duration::from_secs(timeout))
                .connect_timeout(std::time::Duration::from_secs(10))
                .default_headers(headers);
            let builder = zeroclaw_config::schema::apply_runtime_proxy_to_builder(
                builder,
                "model_provider.compatible",
            );

            return builder.build().unwrap_or_else(|error| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"error": super::format_error_chain(&error)})
                        ),
                    "Failed to build proxied timeout client with custom headers: "
                );
                Client::new()
            });
        }

        zeroclaw_config::schema::build_runtime_proxy_client_with_timeouts(
            "model_provider.compatible",
            timeout,
            10,
        )
    }

    /// HTTP client for streaming SSE connections — connect timeout only, no total timeout.
    /// reqwest's total timeout kills long-running streams mid-response; streaming paths must
    /// use this client instead of http_client().
    fn streaming_http_client(&self) -> Client {
        let has_user_agent = self.user_agent.is_some();
        let has_extra_headers = !self.extra_headers.is_empty();

        if has_user_agent || has_extra_headers {
            let mut headers = HeaderMap::new();
            if let Some(ua) = self.user_agent.as_deref()
                && let Ok(value) = HeaderValue::from_str(ua)
            {
                headers.insert(USER_AGENT, value);
            }
            for (key, value) in &self.extra_headers {
                match (
                    reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                    HeaderValue::from_str(value),
                ) {
                    (Ok(name), Ok(val)) => {
                        headers.insert(name, val);
                    }
                    _ => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"header": key})),
                            "Skipping invalid extra header name or value"
                        );
                    }
                }
            }

            let builder = Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .default_headers(headers);
            let builder = zeroclaw_config::schema::apply_runtime_proxy_to_builder(
                builder,
                "provider.compatible",
            );
            return builder.build().unwrap_or_else(|error| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"error": super::format_error_chain(&error)})
                        ),
                    "Failed to build proxied streaming client with custom headers: "
                );
                Client::new()
            });
        }

        let builder = Client::builder().connect_timeout(std::time::Duration::from_secs(10));
        let builder =
            zeroclaw_config::schema::apply_runtime_proxy_to_builder(builder, "provider.compatible");
        builder.build().unwrap_or_else(|error| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": super::format_error_chain(&error)})),
                "Failed to build proxied streaming client: "
            );
            Client::new()
        })
    }

    /// Build the full URL for chat completions, detecting if base_url already includes the path.
    /// This allows custom model_providers with non-standard endpoints (e.g., VolcEngine ARK uses
    /// `/api/coding/v3/chat/completions` instead of `/v1/chat/completions`).
    fn chat_completions_url(&self) -> String {
        // If a custom api_path is configured, use it directly.
        if let Some(ref api_path) = self.api_path {
            let separator = if api_path.starts_with('/') { "" } else { "/" };
            return format!("{}{separator}{api_path}", self.base_url);
        }

        let has_full_endpoint = reqwest::Url::parse(&self.base_url)
            .map(|url| {
                url.path()
                    .trim_end_matches('/')
                    .ends_with("/chat/completions")
            })
            .unwrap_or_else(|_| {
                self.base_url
                    .trim_end_matches('/')
                    .ends_with("/chat/completions")
            });

        if has_full_endpoint {
            self.base_url.clone()
        } else {
            format!("{}/chat/completions", self.base_url)
        }
    }

    fn requires_tool_stream(&self) -> bool {
        let host_requires_tool_stream = reqwest::Url::parse(&self.base_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
            .is_some_and(|host| host == "api.z.ai" || host.ends_with(".z.ai"));

        host_requires_tool_stream || matches!(self.name.as_str(), "zai" | "z.ai")
    }

    fn tool_stream_for_tools(&self, has_tools: bool) -> Option<bool> {
        if has_tools && self.requires_tool_stream() {
            Some(true)
        } else {
            None
        }
    }

    /// Returns true if the given model requires system messages to be merged
    /// into the first user message because its prompt template cannot handle
    /// the `system` role reliably (e.g. DeepSeek V3.2 Jinja rendering errors).
    fn model_requires_system_merge(model: &str) -> bool {
        let id = model
            .rsplit('/')
            .next()
            .unwrap_or(model)
            .to_ascii_lowercase();
        id.contains("deepseek-v3") || id.contains("deepseek_v3")
    }

    /// Whether system messages should be flattened into the first user message,
    /// either because the model_provider was configured that way or the model requires it.
    fn effective_merge_system(&self, model: &str) -> bool {
        self.merge_system_into_user || Self::model_requires_system_merge(model)
    }

    fn reasoning_effort_for_model(&self, model: &str) -> Option<String> {
        let effort = self.reasoning_effort.as_ref()?;
        let id = model
            .rsplit('/')
            .next()
            .unwrap_or(model)
            .to_ascii_lowercase();
        let is_openai_reasoning_model = id == "o1"
            || id.starts_with("o1-")
            || id == "o3"
            || id.starts_with("o3-")
            || id == "o4"
            || id.starts_with("o4-")
            || id.starts_with("gpt-5");
        let is_likely_codex_supported = id.contains("codex") && id.starts_with("gpt-");

        (is_openai_reasoning_model || is_likely_codex_supported).then(|| effort.clone())
    }
}

/// Kimi K2.5/K2.6 models enforce fixed temperatures per mode (1.0 thinking,
/// 0.6 instant) and reject any other value with HTTP 400. Omit `temperature`
/// for kimi-k2.* models so the backend chooses the correct mode default.
/// Substring match covers k2.5, k2.6, and future k2.x variants.
fn compatible_model_omits_temperature(model: &str) -> bool {
    model.contains("kimi-k2")
}

#[derive(Debug, Serialize)]
struct ApiChatRequest {
    model: String,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptionsBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

/// OpenAI-compatible `stream_options.include_usage` toggle.
/// When set with streaming, providers emit a final SSE chunk carrying usage
/// counts (prompt_tokens / completion_tokens) so the agent can populate cost
/// records and the WebSocket done frame for streaming responses.
#[derive(Debug, Serialize, Clone, Copy)]
struct StreamOptionsBody {
    include_usage: bool,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: MessageContent,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Parts(Vec<MessagePart>),
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum MessagePart {
    Text { text: String },
    ImageUrl { image_url: ImageUrlPart },
}

#[derive(Debug, Serialize)]
struct ImageUrlPart {
    url: String,
}

#[derive(Debug, Deserialize)]
struct ApiChatResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<UsageInfo>,
}

#[derive(Debug, Deserialize)]
struct UsageInfo {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

/// Remove `<think>...</think>` blocks from model output.
/// Some reasoning models (e.g. MiniMax) embed their chain-of-thought inline
/// in the `content` field rather than a separate `reasoning_content` field.
/// The resulting `<think>` tags must be stripped before returning to the user.
fn strip_think_tags(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        if let Some(start) = rest.find("<think>") {
            result.push_str(&rest[..start]);
            if let Some(end) = rest[start..].find("</think>") {
                rest = &rest[start + end + "</think>".len()..];
            } else {
                // Unclosed tag: drop the rest to avoid leaking partial reasoning.
                break;
            }
        } else {
            result.push_str(rest);
            break;
        }
    }
    result.trim().to_string()
}

/// OpenAI Chat Completions may return assistant `message.content` as a string,
/// null, or an array of typed parts. Normalize it before storing the internal
/// response shape so compatible gateways that preserve typed parts still work,
/// while unsupported top-level content shapes still fail deserialization.
fn openai_assistant_content_plaintext(content: Option<OpenAiAssistantContent>) -> Option<String> {
    match content? {
        OpenAiAssistantContent::Text(s) => {
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }
        OpenAiAssistantContent::Parts(parts) => {
            let mut text = String::new();
            for part in parts {
                if part.kind.as_deref() != Some("text") {
                    continue;
                }
                let Some(part_text) = part.text.filter(|text| !text.is_empty()) else {
                    continue;
                };
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&part_text);
            }

            if text.is_empty() { None } else { Some(text) }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OpenAiAssistantContent {
    Text(String),
    Parts(Vec<OpenAiAssistantContentPart>),
}

#[derive(Debug, Deserialize)]
struct OpenAiAssistantContentPart {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(from = "RawResponseMessage")]
struct ResponseMessage {
    content: Option<String>,
    /// Reasoning/thinking models (e.g. Qwen3, GLM-4) may return their output
    /// in `reasoning_content` instead of `content`. Used as automatic fallback.
    ///
    /// OpenRouter and vLLM (>= v0.16.0) emit reasoning under `reasoning`
    /// rather than `reasoning_content`. Both keys are accepted on deserialization
    /// via `RawResponseMessage`; when both appear in the same payload, the
    /// canonical `reasoning_content` wins. See #6584.
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<ToolCall>>,
}

/// Intermediate shape for `ResponseMessage` that accepts both
/// `reasoning_content` (canonical) and `reasoning` (OpenRouter / vLLM alias)
/// as distinct fields. `#[serde(alias)]` cannot be used here because serde
/// rejects payloads carrying both keys as a duplicate-field error before any
/// precedence rule can run. By naming the two keys to separate destination
/// fields we let the precedence rule live in `From<RawResponseMessage>`. See
/// #6584 and review feedback on PR #6615.
#[derive(Debug, Deserialize)]
struct RawResponseMessage {
    #[serde(default)]
    content: Option<OpenAiAssistantContent>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
}

impl From<RawResponseMessage> for ResponseMessage {
    fn from(raw: RawResponseMessage) -> Self {
        // Canonical field wins when both are present; the alias fills in only
        // when the canonical name is absent or null.
        let reasoning_content = raw.reasoning_content.or(raw.reasoning);
        ResponseMessage {
            content: openai_assistant_content_plaintext(raw.content),
            reasoning_content,
            tool_calls: raw.tool_calls,
        }
    }
}

impl ResponseMessage {
    /// Extract text content, falling back to `reasoning_content` when `content`
    /// is missing or empty. Reasoning/thinking models (Qwen3, GLM-4, etc.)
    /// often return their output solely in `reasoning_content`.
    /// Strips `<think>...</think>` blocks that some models (e.g. MiniMax) embed
    /// inline in `content` instead of using a separate field.
    fn effective_content(&self) -> String {
        if let Some(content) = self.content.as_ref().filter(|c| !c.is_empty()) {
            let stripped = strip_think_tags(content);
            if !stripped.is_empty() {
                return stripped;
            }
        }

        self.reasoning_content
            .as_ref()
            .map(|c| strip_think_tags(c))
            .filter(|c| !c.is_empty())
            .unwrap_or_default()
    }

    fn effective_content_optional(&self) -> Option<String> {
        if let Some(content) = self.content.as_ref().filter(|c| !c.is_empty()) {
            let stripped = strip_think_tags(content);
            if !stripped.is_empty() {
                return Some(stripped);
            }
        }

        self.reasoning_content
            .as_ref()
            .map(|c| strip_think_tags(c))
            .filter(|c| !c.is_empty())
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct ToolCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "type")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    function: Option<Function>,

    // Compatibility: Some model_providers (e.g., older GLM) may use 'name' directly
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    arguments: Option<String>,

    // Compatibility: DeepSeek sometimes wraps arguments differently
    #[serde(
        rename = "parameters",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    parameters: Option<serde_json::Value>,

    /// See [`zeroclaw_api::ToolCall::extra_content`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    extra_content: Option<serde_json::Value>,
}

impl ToolCall {
    /// Extract function name with fallback logic for various model_provider formats
    fn function_name(&self) -> Option<String> {
        // Standard OpenAI format: tool_calls[].function.name
        if let Some(ref func) = self.function
            && let Some(ref name) = func.name
        {
            return Some(name.clone());
        }
        // Fallback: direct name field
        self.name.clone()
    }

    /// Extract arguments with fallback logic and type conversion
    fn function_arguments(&self) -> Option<String> {
        // Standard OpenAI format: tool_calls[].function.arguments (string)
        if let Some(ref func) = self.function
            && let Some(ref args) = func.arguments
        {
            return Some(args.clone());
        }
        // Fallback: direct arguments field
        if let Some(ref args) = self.arguments {
            return Some(args.clone());
        }
        // Compatibility: Some model_providers return parameters as object instead of string
        if let Some(ref params) = self.parameters {
            return serde_json::to_string(params).ok();
        }
        None
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct Function {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Serialize)]
struct NativeChatRequest {
    model: String,
    messages: Vec<NativeMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    /// Mirrors `ApiChatRequest::stream_options`. Without this, tool-enabled
    /// streaming requests omit `stream_options.include_usage` and OpenAI-
    /// compatible providers never send the final `usage` SSE event — leaving
    /// `/ws/chat` with no token-usage signal whenever native tools are active
    /// (which is the normal gateway path). / #6159.
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptionsBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Debug, Serialize)]
struct NativeMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<MessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCall>>,
    /// Raw reasoning content from thinking models; pass-through for model_providers
    /// that require it in assistant tool-call history messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
}

// ---------------------------------------------------------------
// Streaming support (SSE parser)
// ---------------------------------------------------------------

/// Server-Sent Event stream chunk for OpenAI-compatible streaming.
#[derive(Debug, Deserialize)]
struct StreamChunkResponse {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    /// Final-chunk usage counts. Populated only when the request includes
    /// `stream_options.include_usage: true` and the provider supports it.
    #[serde(default)]
    usage: Option<UsageInfo>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: StreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default)]
struct StreamDelta {
    content: Option<String>,
    /// Reasoning/thinking models may stream output via `reasoning_content`.
    /// OpenRouter and vLLM (>= v0.16.0) emit reasoning deltas under
    /// `reasoning`. Both keys are accepted via `RawStreamDelta`; when both
    /// appear in the same delta, the canonical `reasoning_content` wins. See
    /// #6584 and review feedback on PR #6615.
    reasoning_content: Option<String>,
    /// Native tool-calling deltas in OpenAI chat-completions streaming format.
    tool_calls: Option<Vec<StreamToolCallDelta>>,
}

/// Intermediate shape for `StreamDelta` — same rationale as
/// `RawResponseMessage`: serde rejects payloads that carry both
/// `reasoning_content` and `reasoning` when they target one field via
/// `#[serde(alias)]`, so the two keys must deserialize into separate fields
/// and a precedence rule must merge them.
#[derive(Debug, Deserialize, Default)]
struct RawStreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<StreamToolCallDelta>>,
}

impl<'de> Deserialize<'de> for StreamDelta {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawStreamDelta::deserialize(deserializer)?;
        Ok(StreamDelta {
            content: raw.content,
            reasoning_content: raw.reasoning_content.or(raw.reasoning),
            tool_calls: raw.tool_calls,
        })
    }
}

#[derive(Debug, Deserialize)]
struct StreamToolCallDelta {
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<StreamFunctionDelta>,
    // Compatibility: some model_providers stream name/arguments at top-level.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
    #[serde(default)]
    extra_content: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct StreamFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Default)]
struct StreamToolCallAccumulator {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
    extra_content: Option<serde_json::Value>,
}

impl StreamToolCallAccumulator {
    fn apply_delta(&mut self, delta: &StreamToolCallDelta) {
        if let Some(id) = delta.id.as_ref().filter(|value| !value.is_empty()) {
            self.id = Some(id.clone());
        }

        let delta_name = delta
            .function
            .as_ref()
            .and_then(|function| function.name.as_ref())
            .or(delta.name.as_ref())
            .filter(|value| !value.is_empty());
        if let Some(name) = delta_name {
            self.name = Some(name.clone());
        }

        if let Some(arguments_delta) = delta
            .function
            .as_ref()
            .and_then(|function| function.arguments.as_ref())
            .or(delta.arguments.as_ref())
            .filter(|value| !value.is_empty())
        {
            self.arguments.push_str(arguments_delta);
        }

        // Last-write-wins: signature is opaque and delivered once per call.
        if let Some(extra) = delta.extra_content.as_ref() {
            self.extra_content = Some(extra.clone());
        }
    }

    fn into_provider_tool_call(
        self,
        targets_mistral_tool_call_contract: bool,
        used_tool_call_ids: &mut std::collections::HashSet<String>,
    ) -> Option<ProviderToolCall> {
        let name = self.name?;
        let arguments = if self.arguments.trim().is_empty() {
            "{}".to_string()
        } else {
            self.arguments
        };
        let normalized_arguments = if serde_json::from_str::<serde_json::Value>(&arguments).is_ok()
        {
            arguments
        } else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"function": name, "arguments": arguments})),
                "Invalid JSON in streamed native tool-call arguments, using empty object"
            );
            "{}".to_string()
        };

        Some(ProviderToolCall {
            id: reserve_tool_call_id_for_contract(
                targets_mistral_tool_call_contract,
                self.id,
                used_tool_call_ids,
            ),
            name,
            arguments: normalized_arguments,
            extra_content: self.extra_content,
        })
    }
}

fn parse_sse_chunk(line: &str) -> StreamResult<Option<StreamChunkResponse>> {
    let line = line.trim();

    if line.is_empty() || line.starts_with(':') {
        return Ok(None);
    }

    let Some(data) = line.strip_prefix("data:") else {
        return Ok(None);
    };
    let data = data.trim();

    if data == "[DONE]" {
        return Ok(None);
    }

    serde_json::from_str(data)
        .map(Some)
        .map_err(StreamError::Json)
}

/// Parse custom proxy tool events from SSE lines.
/// These are emitted by proxies like claude-max-api-proxy that execute tools
/// internally and forward observability events via custom SSE fields.
fn parse_proxy_tool_event(line: &str) -> Option<StreamEvent> {
    let data = line.trim().strip_prefix("data:")?.trim();
    let obj: serde_json::Value = serde_json::from_str(data).ok()?;

    if let Some(ts) = obj.get("x_tool_start") {
        let Some(name) = ts.get("name").and_then(|v| v.as_str()) else {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "proxy x_tool_start event missing required 'name' field"
            );
            return None;
        };
        let name = name.to_string();
        let args = ts
            .get("arguments")
            .and_then(|v| v.as_str())
            .unwrap_or("{}")
            .to_string();
        return Some(StreamEvent::PreExecutedToolCall { name, args });
    }

    if let Some(tr) = obj.get("x_tool_result") {
        let name = tr
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let output = tr
            .get("output")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        return Some(StreamEvent::PreExecutedToolResult { name, output });
    }

    None
}

fn extract_sse_text_delta(choice: &StreamChoice) -> Option<String> {
    if let Some(content) = &choice.delta.content
        && !content.is_empty()
    {
        return Some(content.clone());
    }

    None
}

fn extract_sse_reasoning_delta(choice: &StreamChoice) -> Option<String> {
    choice
        .delta
        .reasoning_content
        .as_ref()
        .filter(|value| !value.is_empty())
        .cloned()
}

fn is_valid_mistral_tool_call_id(id: &str) -> bool {
    id.len() == 9 && id.chars().all(|c| c.is_ascii_alphanumeric())
}

fn reserve_tool_call_id_for_contract(
    targets_mistral_tool_call_contract: bool,
    raw_id: Option<String>,
    used_ids: &mut std::collections::HashSet<String>,
) -> String {
    if !targets_mistral_tool_call_contract {
        let id = raw_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if used_ids.insert(id.clone()) {
            return id;
        }

        loop {
            let candidate = uuid::Uuid::new_v4().to_string();
            if used_ids.insert(candidate.clone()) {
                return candidate;
            }
        }
    }

    if let Some(id) = raw_id.as_deref()
        && is_valid_mistral_tool_call_id(id)
        && used_ids.insert(id.to_string())
    {
        return id.to_string();
    }

    let mut candidate = raw_id
        .as_deref()
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(9)
        .collect::<String>();

    if candidate.len() < 9 {
        candidate.extend(
            uuid::Uuid::new_v4()
                .as_simple()
                .to_string()
                .chars()
                .take(9 - candidate.len()),
        );
    }

    if used_ids.insert(candidate.clone()) {
        return candidate;
    }

    loop {
        let generated = uuid::Uuid::new_v4()
            .as_simple()
            .to_string()
            .chars()
            .take(9)
            .collect::<String>();
        if used_ids.insert(generated.clone()) {
            return generated;
        }
    }
}

/// Parse SSE (Server-Sent Events) stream from OpenAI-compatible model_providers.
/// Handles the `data: {...}` format and `[DONE]` sentinel.
///
/// Returns a `StreamChunk` that distinguishes content from reasoning:
/// - Content deltas → `StreamChunk::delta`
/// - Reasoning deltas → `StreamChunk::reasoning`
fn parse_sse_line(line: &str) -> StreamResult<Option<StreamChunk>> {
    let chunk = match parse_sse_chunk(line)? {
        Some(c) => c,
        None => return Ok(None),
    };

    if let Some(choice) = chunk.choices.first() {
        if let Some(content) = &choice.delta.content
            && !content.is_empty()
        {
            return Ok(Some(StreamChunk::delta(content.clone())));
        }
        if let Some(reasoning) = &choice.delta.reasoning_content
            && !reasoning.is_empty()
        {
            return Ok(Some(StreamChunk::reasoning(reasoning.clone())));
        }
    }

    Ok(None)
}

/// Convert SSE byte stream to text chunks.
fn sse_bytes_to_chunks(
    response: reqwest::Response,
    count_tokens: bool,
) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamChunk>>(100);

    let handle = ::zeroclaw_spawn::spawn!(async move {
        let mut buffer = String::new();

        match response.error_for_status_ref() {
            Ok(_) => {}
            Err(e) => {
                let _ = tx
                    .send(Err(StreamError::Http(super::format_error_chain(&e))))
                    .await;
                return;
            }
        }

        let mut bytes_stream = response.bytes_stream();
        // Accumulate partial UTF-8 sequences that may be split across
        // HTTP/1.1 chunked transfer boundaries (e.g. 3-byte CJK chars).
        let mut utf8_buf: Vec<u8> = Vec::new();

        while let Some(item) = bytes_stream.next().await {
            match item {
                Ok(bytes) => {
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
                                // Could still be an incomplete multi-byte char; wait for more data
                                continue;
                            }
                            let valid =
                                String::from_utf8_lossy(&utf8_buf[..valid_up_to]).into_owned();
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

                        match parse_sse_line(&line) {
                            Ok(Some(chunk)) => {
                                let chunk = if count_tokens {
                                    chunk.with_token_estimate()
                                } else {
                                    chunk
                                };
                                if tx.send(Ok(chunk)).await.is_err() {
                                    return; // Receiver dropped
                                }
                            }
                            Ok(None) => {}
                            Err(e) => {
                                let _ = tx.send(Err(e)).await;
                                return;
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = tx
                        .send(Err(StreamError::Http(super::format_error_chain(&e))))
                        .await;
                    return;
                }
            }
        }

        let _ = tx.send(Ok(StreamChunk::final_chunk())).await;
    });

    let guard = AbortOnDrop::new(handle.abort_handle());
    stream::unfold((rx, guard), |(mut rx, guard)| async {
        rx.recv().await.map(|chunk| (chunk, (rx, guard)))
    })
    .boxed()
}

/// Convert SSE byte stream to structured streaming events.
pub(crate) fn sse_bytes_to_events(
    response: reqwest::Response,
    count_tokens: bool,
) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
    sse_bytes_to_events_for_contract(response, count_tokens, false)
}

fn sse_bytes_to_events_for_contract(
    response: reqwest::Response,
    count_tokens: bool,
    targets_mistral_tool_call_contract: bool,
) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(100);

    let handle = ::zeroclaw_spawn::spawn!(async move {
        let mut buffer = String::new();
        let mut tool_calls: Vec<StreamToolCallAccumulator> = Vec::new();
        let mut used_tool_call_ids = std::collections::HashSet::new();
        let mut emitted_tool_calls = false;

        match response.error_for_status_ref() {
            Ok(_) => {}
            Err(e) => {
                let _ = tx
                    .send(Err(StreamError::Http(super::format_error_chain(&e))))
                    .await;
                return;
            }
        }

        let mut bytes_stream = response.bytes_stream();
        // Accumulate partial UTF-8 sequences split across chunk boundaries.
        let mut utf8_buf: Vec<u8> = Vec::new();
        while let Some(item) = bytes_stream.next().await {
            match item {
                Ok(bytes) => {
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
                            let valid =
                                String::from_utf8_lossy(&utf8_buf[..valid_up_to]).into_owned();
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

                        // Custom proxy events for pre-executed tool calls
                        // (e.g. claude-max-api-proxy streaming x_tool_start/x_tool_result)
                        if let Some(event) = parse_proxy_tool_event(&line) {
                            if tx.send(Ok(event)).await.is_err() {
                                return;
                            }
                            continue;
                        }

                        let chunk = match parse_sse_chunk(&line) {
                            Ok(Some(chunk)) => chunk,
                            Ok(None) => continue,
                            Err(e) => {
                                let _ = tx.send(Err(e)).await;
                                return;
                            }
                        };

                        let mut should_emit_tool_calls = false;
                        for choice in &chunk.choices {
                            if let Some(reasoning_delta) = extract_sse_reasoning_delta(choice) {
                                let reasoning_chunk = StreamChunk::reasoning(reasoning_delta);
                                if tx
                                    .send(Ok(StreamEvent::TextDelta(reasoning_chunk)))
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            if let Some(text_delta) = extract_sse_text_delta(choice) {
                                let mut text_chunk = StreamChunk::delta(text_delta);
                                if count_tokens {
                                    text_chunk = text_chunk.with_token_estimate();
                                }
                                if tx
                                    .send(Ok(StreamEvent::TextDelta(text_chunk)))
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                            }

                            if let Some(deltas) = choice.delta.tool_calls.as_ref() {
                                for delta in deltas {
                                    let index = delta.index.unwrap_or(tool_calls.len());
                                    if index >= tool_calls.len() {
                                        tool_calls.resize_with(index + 1, Default::default);
                                    }
                                    if let Some(acc) = tool_calls.get_mut(index) {
                                        acc.apply_delta(delta);
                                    }
                                }
                            }

                            if choice.finish_reason.as_deref() == Some("tool_calls") {
                                should_emit_tool_calls = true;
                            }
                        }

                        if let Some(usage) = chunk.usage.as_ref() {
                            let token_usage = zeroclaw_api::model_provider::TokenUsage {
                                input_tokens: usage.prompt_tokens,
                                output_tokens: usage.completion_tokens,
                                cached_input_tokens: None,
                            };
                            if tx.send(Ok(StreamEvent::Usage(token_usage))).await.is_err() {
                                return;
                            }
                        }

                        if should_emit_tool_calls && !emitted_tool_calls {
                            emitted_tool_calls = true;
                            for tool_call in tool_calls.drain(..).filter_map(|tool_call| {
                                tool_call.into_provider_tool_call(
                                    targets_mistral_tool_call_contract,
                                    &mut used_tool_call_ids,
                                )
                            }) {
                                if tx.send(Ok(StreamEvent::ToolCall(tool_call))).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = tx
                        .send(Err(StreamError::Http(super::format_error_chain(&e))))
                        .await;
                    return;
                }
            }
        }

        if !emitted_tool_calls {
            for tool_call in tool_calls.drain(..).filter_map(|tool_call| {
                tool_call.into_provider_tool_call(
                    targets_mistral_tool_call_contract,
                    &mut used_tool_call_ids,
                )
            }) {
                if tx.send(Ok(StreamEvent::ToolCall(tool_call))).await.is_err() {
                    return;
                }
            }
        }

        let _ = tx.send(Ok(StreamEvent::Final)).await;
    });

    let guard = AbortOnDrop::new(handle.abort_handle());
    stream::unfold((rx, guard), |(mut rx, guard)| async move {
        rx.recv().await.map(|event| (event, (rx, guard)))
    })
    .boxed()
}

fn parse_chat_response_body(name: &str, body: &str) -> anyhow::Result<ApiChatResponse> {
    serde_json::from_str(body).map_err(|_| {
        let sanitized = super::sanitize_api_error(body);
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "model_provider": name,
                    "body": &sanitized,
                })),
            "compatible: unexpected chat-completions payload"
        );
        anyhow::Error::msg(format!(
            "{name} API returned an unexpected chat-completions payload; body={sanitized}"
        ))
    })
}

impl OpenAiCompatibleModelProvider {
    fn apply_auth_header(
        &self,
        req: reqwest::RequestBuilder,
        credential: Option<&str>,
    ) -> reqwest::RequestBuilder {
        apply_auth_to_request(req, &self.auth_header, credential)
    }

    fn convert_tool_specs(
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
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": params,
                        }
                    })
                })
                .collect()
        })
    }

    /// Wrap [`Self::convert_tool_specs`] with the per-model conservative
    /// sanitizer when the provider opted in via
    /// [`Self::with_local_model_tool_sanitize`] AND the runtime model id
    /// matches a known-troubled family (today: gemma-4 on llama.cpp; also
    /// the empty-model case where the operator hasn't named one).
    fn convert_tool_specs_for_model(
        &self,
        tools: Option<&[zeroclaw_api::tool::ToolSpec]>,
        model: &str,
    ) -> Option<Vec<serde_json::Value>> {
        let converted = Self::convert_tool_specs(tools)?;
        if !self.local_model_tool_sanitize || !Self::should_sanitize_local_tool_schema(model) {
            return Some(converted);
        }
        Some(
            converted
                .into_iter()
                .map(|mut tool| {
                    let Some(raw_parameters) = tool.get("parameters").cloned() else {
                        return tool;
                    };
                    let cleaned = zeroclaw_api::schema::SchemaCleanr::clean(
                        raw_parameters,
                        zeroclaw_api::schema::CleaningStrategy::Conservative,
                    );
                    if let Some(obj) = tool.as_object_mut() {
                        obj.insert("parameters".to_string(), cleaned);
                    }
                    tool
                })
                .collect(),
        )
    }

    fn should_sanitize_local_tool_schema(model: &str) -> bool {
        let lower = model.to_ascii_lowercase();
        model.is_empty() || lower.contains("gemma-4") || lower.contains("gemma4")
    }

    fn build_native_tool_chat_request(
        &self,
        effective_messages: &[ChatMessage],
        tools: Option<Vec<serde_json::Value>>,
        model: &str,
        temperature: Option<f64>,
        allow_user_image_parts: bool,
    ) -> NativeChatRequest {
        let has_tool_entries = tools.as_ref().is_some_and(|tools| !tools.is_empty());
        let tool_choice = tools.as_ref().map(|_| "auto".to_string());

        NativeChatRequest {
            model: model.to_string(),
            messages: self.convert_messages_for_native(effective_messages, allow_user_image_parts),
            temperature,
            stream: Some(false),
            // Non-streaming path; `usage` is on the final response body, not
            // gated on `stream_options.include_usage`.
            stream_options: None,
            reasoning_effort: self.reasoning_effort_for_model(model),
            tool_stream: self.tool_stream_for_tools(has_tool_entries),
            tools,
            tool_choice,
            max_tokens: self.max_tokens,
        }
    }

    /// Normalize local file paths and remote URLs inside `[IMAGE:…]` markers
    /// to base64 data URIs before any message reaches the upstream provider.
    ///
    /// OpenAI-compatible backends (vLLM, llama.cpp server, LM Studio, etc.) run
    /// on a different host than zeroclaw in typical deployments, so a marker
    /// containing a host-local file path (e.g. `[IMAGE:/home/u/.../photo.jpg]`)
    /// would otherwise reach `to_message_content`, be promoted to a
    /// `MessagePart::ImageUrl`, and arrive at the backend as
    /// `image_url.url = "/home/u/.../photo.jpg"` (strict servers reject this:
    /// vLLM 0.20+ returns `"The URL must be either a HTTP, data or file URL."`).
    /// See issue #6399.
    ///
    /// The agent loop normalizes messages once before calling `chat`, but
    /// auxiliary paths (delegate sub-agents, context compression, plain
    /// `chat_with_system` callers) do not. Normalizing at the provider
    /// boundary makes the contract uniform regardless of caller.
    async fn normalize_messages_for_upstream(
        messages: &[ChatMessage],
    ) -> anyhow::Result<Vec<ChatMessage>> {
        let config = zeroclaw_config::schema::MultimodalConfig::default();
        let prepared = multimodal::prepare_messages_for_provider(messages, &config).await?;
        Ok(prepared.messages)
    }

    fn to_message_content(
        role: &str,
        content: &str,
        allow_user_image_parts: bool,
    ) -> MessageContent {
        if role != "user" || !allow_user_image_parts {
            return MessageContent::Text(content.to_string());
        }

        let (cleaned_text, image_refs) = multimodal::parse_image_markers(content);
        if image_refs.is_empty() {
            return MessageContent::Text(content.to_string());
        }

        let mut parts = Vec::with_capacity(image_refs.len() + 1);
        let trimmed_text = cleaned_text.trim();
        if !trimmed_text.is_empty() {
            parts.push(MessagePart::Text {
                text: trimmed_text.to_string(),
            });
        }

        for image_ref in image_refs {
            parts.push(MessagePart::ImageUrl {
                image_url: ImageUrlPart { url: image_ref },
            });
        }

        MessageContent::Parts(parts)
    }

    fn convert_messages_for_native(
        &self,
        messages: &[ChatMessage],
        allow_user_image_parts: bool,
    ) -> Vec<NativeMessage> {
        let targets_mistral_tool_call_contract = self.targets_mistral_tool_call_contract();
        let mut used_tool_call_ids = std::collections::HashSet::new();
        let mut tool_call_id_map = std::collections::HashMap::new();

        messages
            .iter()
            .map(|message| {
                if message.role == "assistant"
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content)
                    && let Some(tool_calls_value) = value.get("tool_calls")
                    && let Ok(parsed_calls) =
                        serde_json::from_value::<Vec<ProviderToolCall>>(tool_calls_value.clone())
                {
                    let tool_calls = parsed_calls
                        .into_iter()
                        .map(|tc| ToolCall {
                            id: Some({
                                let normalized_id = reserve_tool_call_id_for_contract(
                                    targets_mistral_tool_call_contract,
                                    Some(tc.id.clone()),
                                    &mut used_tool_call_ids,
                                );
                                tool_call_id_map.insert(tc.id, normalized_id.clone());
                                normalized_id
                            }),
                            kind: Some("function".to_string()),
                            function: Some(Function {
                                name: Some(tc.name),
                                arguments: Some(tc.arguments),
                            }),
                            name: None,
                            arguments: None,
                            parameters: None,
                            // Round-trip extra_content (e.g. Gemini
                            // thoughtSignature) — dropping it here was the bug.
                            extra_content: tc.extra_content,
                        })
                        .collect::<Vec<_>>();

                    let content = value
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .map(|value| MessageContent::Text(value.to_string()));

                    // Accept both `reasoning_content` (canonical) and
                    // `reasoning` (OpenRouter / vLLM >= v0.16.0). See #6584.
                    let reasoning_content = value
                        .get("reasoning_content")
                        .or_else(|| value.get("reasoning"))
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string);

                    return NativeMessage {
                        role: "assistant".to_string(),
                        content,
                        tool_call_id: None,
                        tool_calls: Some(tool_calls),
                        reasoning_content,
                    };
                }

                // Plain-text assistant turns from thinking-mode providers carry
                // `reasoning_content` in a JSON-encoded `content` field with no
                // `tool_calls` key. Without this branch the message would fall
                // through to the plain-text fallback below and lose
                // `reasoning_content`, so the next request to providers that
                // require reasoning round-trip (e.g. DeepSeek V4 thinking) is
                // rejected with a 400. See #6233.
                if message.role == "assistant"
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content)
                    && value.get("tool_calls").is_none()
                    && let Some(reasoning_content) = value
                        .get("reasoning_content")
                        .and_then(serde_json::Value::as_str)
                    && matches!(
                        value.get("content"),
                        None | Some(serde_json::Value::Null | serde_json::Value::String(_))
                    )
                {
                    let content = value
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .map(|value| MessageContent::Text(value.to_string()));

                    return NativeMessage {
                        role: "assistant".to_string(),
                        content,
                        tool_call_id: None,
                        tool_calls: None,
                        reasoning_content: Some(reasoning_content.to_string()),
                    };
                }

                if message.role == "tool"
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content)
                {
                    let tool_call_id = value
                        .get("tool_call_id")
                        .and_then(serde_json::Value::as_str)
                        .map(|raw_id| {
                            tool_call_id_map.get(raw_id).cloned().unwrap_or_else(|| {
                                let normalized_id = reserve_tool_call_id_for_contract(
                                    targets_mistral_tool_call_contract,
                                    Some(raw_id.to_string()),
                                    &mut used_tool_call_ids,
                                );
                                tool_call_id_map.insert(raw_id.to_string(), normalized_id.clone());
                                normalized_id
                            })
                        });
                    let content = value
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .map(|value| MessageContent::Text(value.to_string()))
                        .or_else(|| Some(MessageContent::Text(message.content.clone())));

                    return NativeMessage {
                        role: "tool".to_string(),
                        content,
                        tool_call_id,
                        tool_calls: None,
                        reasoning_content: None,
                    };
                }

                NativeMessage {
                    role: message.role.clone(),
                    content: Some(Self::to_message_content(
                        &message.role,
                        &message.content,
                        allow_user_image_parts,
                    )),
                    tool_call_id: None,
                    tool_calls: None,
                    reasoning_content: None,
                }
            })
            .collect()
    }

    /// Strip native tool-calling constructs from messages for model_providers that
    /// do not support native tool calling (e.g. MiniMax).
    ///
    /// Conversation history may contain tool-role messages and assistant
    /// messages with `tool_calls` JSON from previous sessions or from
    /// model_provider switches.  Sending these to a non-native-tool model_provider
    /// causes hard API errors like MiniMax's
    /// "tool result's tool id not found".
    ///
    /// - **tool-role messages** are dropped entirely.
    /// - **assistant messages with `tool_calls`** are converted to plain
    ///   text by extracting only the `content` field (or dropped when the
    ///   content is empty).
    fn strip_native_tool_messages(&self, messages: &[ChatMessage]) -> Vec<ChatMessage> {
        if self.native_tool_calling {
            return messages.to_vec();
        }
        let intermediate = messages.iter().filter_map(|msg| {
            if msg.role == "tool" {
                return None;
            }
            if msg.role == "assistant"
                && let Ok(value) = serde_json::from_str::<serde_json::Value>(&msg.content)
                && value.get("tool_calls").is_some()
            {
                let text = value
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                return if text.is_empty() {
                    None
                } else {
                    Some(ChatMessage::assistant(&text))
                };
            }
            Some(msg.clone())
        });

        // Coalesce adjacent assistant messages.
        //
        // A typical trace is:
        //     user → assistant{content, tool_calls} → tool{result} → assistant{reply}
        // After the filter_map above the `tool` message is gone and the first
        // assistant has been rewritten to plain text, leaving two assistant
        // messages in a row. Providers targeted by the `native_tool_calling =
        // false` path (Anthropic upstream, MiniMax, and other OpenAI-compat
        // wrappers) reject consecutive same-role messages with HTTP 400, so we
        // merge them here.
        let mut coalesced: Vec<ChatMessage> = Vec::with_capacity(messages.len());
        for msg in intermediate {
            match coalesced.last_mut() {
                Some(last) if last.role == "assistant" && msg.role == "assistant" => {
                    if !last.content.is_empty() && !msg.content.is_empty() {
                        last.content.push_str("\n\n");
                    }
                    last.content.push_str(&msg.content);
                }
                _ => coalesced.push(msg),
            }
        }
        coalesced
    }

    fn with_prompt_guided_tool_instructions(
        messages: &[ChatMessage],
        tools: Option<&[zeroclaw_api::tool::ToolSpec]>,
    ) -> Vec<ChatMessage> {
        let Some(tools) = tools else {
            return messages.to_vec();
        };

        if tools.is_empty() {
            return messages.to_vec();
        }

        let instructions = zeroclaw_api::model_provider::build_tool_instructions_text(tools);
        let mut modified_messages = messages.to_vec();

        if let Some(system_message) = modified_messages.iter_mut().find(|m| m.role == "system") {
            if !system_message.content.is_empty() {
                system_message.content.push_str("\n\n");
            }
            system_message.content.push_str(&instructions);
        } else {
            modified_messages.insert(0, ChatMessage::system(instructions));
        }

        modified_messages
    }

    fn targets_mistral_tool_call_contract(&self) -> bool {
        if self.name.eq_ignore_ascii_case("mistral") {
            return true;
        }

        reqwest::Url::parse(&self.base_url)
            .ok()
            .and_then(|url| url.host_str().map(|h| h.to_ascii_lowercase()))
            .is_some_and(|host| host == "mistral.ai" || host.ends_with(".mistral.ai"))
    }

    fn reserve_tool_call_id(
        &self,
        raw_id: Option<String>,
        used_ids: &mut std::collections::HashSet<String>,
    ) -> String {
        reserve_tool_call_id_for_contract(
            self.targets_mistral_tool_call_contract(),
            raw_id,
            used_ids,
        )
    }

    fn parse_native_response(&self, message: ResponseMessage) -> ProviderChatResponse {
        let text = message.effective_content_optional();
        let reasoning_content = message.reasoning_content.clone();
        let mut used_tool_call_ids = std::collections::HashSet::new();
        let tool_calls = message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .filter_map(|tc| {
                let name = tc.function_name()?;
                let arguments = tc.function_arguments().unwrap_or_else(|| "{}".to_string());
                let normalized_arguments = if serde_json::from_str::<serde_json::Value>(&arguments)
                    .is_ok()
                {
                    arguments
                } else {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(
                                ::serde_json::json!({"function": name, "arguments": arguments})
                            ),
                        "Invalid JSON in native tool-call arguments, using empty object"
                    );
                    "{}".to_string()
                };
                Some(ProviderToolCall {
                    id: self.reserve_tool_call_id(tc.id, &mut used_tool_call_ids),
                    name,
                    arguments: normalized_arguments,
                    extra_content: tc.extra_content,
                })
            })
            .collect::<Vec<_>>();

        ProviderChatResponse {
            text,
            tool_calls,
            usage: None,
            reasoning_content,
        }
    }

    fn is_native_tool_schema_unsupported(status: reqwest::StatusCode, error: &str) -> bool {
        if !matches!(
            status,
            reqwest::StatusCode::BAD_REQUEST | reqwest::StatusCode::UNPROCESSABLE_ENTITY
        ) {
            return false;
        }

        let lower = error.to_lowercase();
        [
            "unknown parameter: tools",
            "unsupported parameter: tools",
            "unrecognized field `tools`",
            "does not support tools",
            "function calling is not supported",
            "tool_choice",
            "tool call validation failed",
            "was not in request",
        ]
        .iter()
        .any(|hint| lower.contains(hint))
    }
}

#[async_trait]
impl ModelProvider for OpenAiCompatibleModelProvider {
    fn capabilities(&self) -> zeroclaw_api::model_provider::ProviderCapabilities {
        zeroclaw_api::model_provider::ProviderCapabilities {
            native_tool_calling: self.native_tool_calling,
            vision: self.supports_vision,
            prompt_caching: false,
            extended_thinking: false,
        }
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        // When a credential is present, hit the model_provider's native /models endpoint
        // (OpenAI-compatible: GET {base_url}/models). Local OpenAI-compatible
        // servers that explicitly allow unauthenticated listing use the same
        // path without an Authorization header.
        let list_credential = self.credential.as_deref();
        if list_credential.is_some() || self.unauthenticated_model_listing {
            let url = format!("{}/models", self.base_url);
            let response = self
                .apply_auth_header(self.http_client().get(&url), list_credential)
                .send()
                .await
                .map_err(|e| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "model_provider": &self.name,
                                "url": &url,
                                "phase": "model_list_request",
                                "error": super::format_error_chain(&e),
                            })),
                        "compatible: model list request failed"
                    );
                    anyhow::Error::msg(format!(
                        "{} model list request failed: {url}: {e}",
                        self.name
                    ))
                })?;
            if !response.status().is_success() {
                let status = response.status();
                anyhow::bail!("{} model list failed at {url}: HTTP {status}", self.name);
            }
            let body: ModelsResponse = response.json().await.map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "model_provider": &self.name,
                            "phase": "model_list_parse",
                            "error": super::format_error_chain(&e),
                        })),
                    "compatible: model list returned invalid JSON"
                );
                anyhow::Error::msg(format!(
                    "{} model list returned invalid JSON: {e}",
                    self.name
                ))
            })?;
            return Ok(normalize_model_ids(body));
        }
        // No credential — try models.dev first, then OpenRouter as a
        // last-resort fallback for vendors that aren't in models.dev.
        if let Some(key) = &self.models_dev_key {
            match crate::models_dev::list_models_for(key).await {
                Ok(models) if !models.is_empty() => return Ok(models),
                Ok(_) => {} // empty → fall through to openrouter
                Err(e) => {
                    if self.openrouter_vendor_prefix.is_none() {
                        return Err(e);
                    }
                }
            }
        }
        match &self.openrouter_vendor_prefix {
            Some(prefix) => crate::openrouter_catalog::list_models_for_vendor(prefix).await,
            None => anyhow::bail!("live model listing is not supported for this model_provider"),
        }
    }

    async fn list_models_with_pricing(
        &self,
    ) -> anyhow::Result<Vec<zeroclaw_api::model_provider::ModelInfo>> {
        use zeroclaw_api::model_provider::ModelInfo;
        // When a credential is present, hit the provider's native /models
        // endpoint — this returns pricing data that we can capture.
        let list_credential = self.credential.as_deref();
        if list_credential.is_some() || self.unauthenticated_model_listing {
            let url = format!("{}/models", self.base_url);
            let response = self
                .apply_auth_header(self.http_client().get(&url), list_credential)
                .send()
                .await
                .map_err(|e| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "model_provider": &self.name,
                                "url": &url,
                                "phase": "model_list_request",
                                "error": super::format_error_chain(&e),
                            })),
                        "compatible: model list request failed"
                    );
                    anyhow::Error::msg(format!(
                        "{} model list request failed: {url}: {e}",
                        self.name
                    ))
                })?;
            if !response.status().is_success() {
                let status = response.status();
                anyhow::bail!("{} model list failed at {url}: HTTP {status}", self.name);
            }
            let body: ModelsResponse = response.json().await.map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "model_provider": &self.name,
                            "phase": "model_list_parse",
                            "error": super::format_error_chain(&e),
                        })),
                    "compatible: model list returned invalid JSON"
                );
                anyhow::Error::msg(format!(
                    "{} model list returned invalid JSON: {e}",
                    self.name
                ))
            })?;
            return Ok(normalize_models_with_pricing(body));
        }
        // No credential — try models.dev first (no pricing from that source),
        // then fall back to OpenRouter which does include pricing.
        if let Some(key) = &self.models_dev_key {
            match crate::models_dev::list_models_for(key).await {
                Ok(models) if !models.is_empty() => {
                    return Ok(models
                        .into_iter()
                        .map(|id| ModelInfo { id, pricing: None })
                        .collect());
                }
                Ok(_) => {} // empty → fall through to openrouter
                Err(_) if self.openrouter_vendor_prefix.is_none() => {
                    return Ok(Vec::new());
                }
                Err(_) => {} // fall through to openrouter
            }
        }
        match &self.openrouter_vendor_prefix {
            Some(prefix) => {
                crate::openrouter_catalog::list_models_for_vendor_with_pricing(prefix).await
            }
            None => Ok(Vec::new()),
        }
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let temperature = if temperature.is_none() && compatible_model_omits_temperature(model) {
            None
        } else {
            Some(temperature.unwrap_or(self.default_temperature()))
        };
        let credential = self.credential.as_deref();

        // Normalize image markers (e.g. local file paths from channel
        // attachments) into base64 data URIs before this message reaches the
        // upstream provider — see issue #6399.
        let user_msg = ChatMessage {
            role: "user".to_string(),
            content: message.to_string(),
        };
        let normalized_user =
            Self::normalize_messages_for_upstream(std::slice::from_ref(&user_msg))
                .await?
                .pop()
                .unwrap_or(user_msg);
        let normalized_message = normalized_user.content;

        let merge = self.effective_merge_system(model);
        let mut messages = Vec::new();

        if merge {
            let content = match system_prompt {
                Some(sys) => format!("{sys}\n\n{normalized_message}"),
                None => normalized_message,
            };
            messages.push(Message {
                role: "user".to_string(),
                content: Self::to_message_content("user", &content, !merge),
            });
        } else {
            if let Some(sys) = system_prompt {
                messages.push(Message {
                    role: "system".to_string(),
                    content: MessageContent::Text(sys.to_string()),
                });
            }
            messages.push(Message {
                role: "user".to_string(),
                content: Self::to_message_content("user", &normalized_message, true),
            });
        }

        let request = ApiChatRequest {
            model: model.to_string(),
            messages,
            temperature,
            stream: Some(false),
            stream_options: None,
            reasoning_effort: self.reasoning_effort_for_model(model),
            tool_stream: None,
            tools: None,
            tool_choice: None,
            max_tokens: self.max_tokens,
        };

        let url = self.chat_completions_url();

        let response = match self
            .apply_auth_header(self.http_client().post(&url).json(&request), credential)
            .send()
            .await
        {
            Ok(response) => response,
            Err(chat_error) => {
                return Err(chat_error.into());
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let error = response.text().await?;
            let sanitized = super::sanitize_api_error(&error);
            anyhow::bail!("{} API error ({status}): {sanitized}", self.name);
        }

        let body = response.text().await?;
        let chat_response = parse_chat_response_body(&self.name, &body)?;

        chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| {
                if c.message.tool_calls.is_some()
                    && c.message
                        .tool_calls
                        .as_ref()
                        .is_some_and(|t: &Vec<_>| !t.is_empty())
                {
                    serde_json::to_string(&c.message)
                        .unwrap_or_else(|_| c.message.effective_content())
                } else {
                    c.message.effective_content()
                }
            })
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"model_provider": &self.name})),
                    "compatible: empty choices in response"
                );
                anyhow::Error::msg(format!("No response from {}", self.name))
            })
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let temperature = if temperature.is_none() && compatible_model_omits_temperature(model) {
            None
        } else {
            Some(temperature.unwrap_or(self.default_temperature()))
        };
        let credential = self.credential.as_deref();

        let normalized = Self::normalize_messages_for_upstream(messages).await?;
        let merge = self.effective_merge_system(model);
        let effective_messages = Self::flatten_system_messages(&normalized, merge);
        // Strip native tool constructs for non-native-tool model_providers.
        let effective_messages = self.strip_native_tool_messages(&effective_messages);
        let api_messages: Vec<Message> = effective_messages
            .iter()
            .map(|m| Message {
                role: m.role.clone(),
                content: Self::to_message_content(&m.role, &m.content, !merge),
            })
            .collect();

        let request = ApiChatRequest {
            model: model.to_string(),
            messages: api_messages,
            temperature,
            stream: Some(false),
            stream_options: None,
            reasoning_effort: self.reasoning_effort_for_model(model),
            tool_stream: None,
            tools: None,
            tool_choice: None,
            max_tokens: self.max_tokens,
        };

        let url = self.chat_completions_url();
        let response = match self
            .apply_auth_header(self.http_client().post(&url).json(&request), credential)
            .send()
            .await
        {
            Ok(response) => response,
            Err(chat_error) => return Err(chat_error.into()),
        };

        if !response.status().is_success() {
            return Err(super::api_error(&self.name, response).await);
        }

        let body = response.text().await?;
        let chat_response = parse_chat_response_body(&self.name, &body)?;

        chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| {
                if c.message.tool_calls.is_some()
                    && c.message
                        .tool_calls
                        .as_ref()
                        .is_some_and(|t: &Vec<_>| !t.is_empty())
                {
                    serde_json::to_string(&c.message)
                        .unwrap_or_else(|_| c.message.effective_content())
                } else {
                    c.message.effective_content()
                }
            })
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"model_provider": &self.name})),
                    "compatible: empty choices in response"
                );
                anyhow::Error::msg(format!("No response from {}", self.name))
            })
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ProviderChatResponse> {
        let temperature = if temperature.is_none() && compatible_model_omits_temperature(model) {
            None
        } else {
            Some(temperature.unwrap_or(self.default_temperature()))
        };
        let credential = self.credential.as_deref();

        let normalized = Self::normalize_messages_for_upstream(messages).await?;
        let merge = self.effective_merge_system(model);
        let effective_messages = Self::flatten_system_messages(&normalized, merge);
        let effective_messages = if self.native_tool_calling {
            effective_messages
        } else {
            self.strip_native_tool_messages(&effective_messages)
        };
        let tools = if tools.is_empty() {
            None
        } else {
            Some(tools.to_vec())
        };
        let request = self.build_native_tool_chat_request(
            &effective_messages,
            tools,
            model,
            temperature,
            !merge,
        );

        let url = self.chat_completions_url();
        let response = match self
            .apply_auth_header(self.http_client().post(&url).json(&request), credential)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "{} native tool call transport failed: {error}; falling back to history path",
                        self.name
                    )
                );
                let text = self.chat_with_history(messages, model, temperature).await?;
                return Ok(ProviderChatResponse {
                    text: Some(text),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                });
            }
        };

        if !response.status().is_success() {
            return Err(super::api_error(&self.name, response).await);
        }

        let body = response.text().await?;
        let chat_response = parse_chat_response_body(&self.name, &body)?;
        let usage = chat_response.usage.map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cached_input_tokens: None,
        });
        let choice = chat_response.choices.into_iter().next().ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"model_provider": &self.name})),
                "compatible: empty choices in response"
            );
            anyhow::Error::msg(format!("No response from {}", self.name))
        })?;

        let text = choice.message.effective_content_optional();
        let reasoning_content = choice.message.reasoning_content;
        let mut used_tool_call_ids = std::collections::HashSet::new();
        let tool_calls = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .filter_map(|tc| {
                let function = tc.function?;
                let name = function.name?;
                let arguments = function.arguments.unwrap_or_else(|| "{}".to_string());
                Some(ProviderToolCall {
                    id: self.reserve_tool_call_id(tc.id, &mut used_tool_call_ids),
                    name,
                    arguments,
                    extra_content: tc.extra_content,
                })
            })
            .collect::<Vec<_>>();

        Ok(ProviderChatResponse {
            text,
            tool_calls,
            usage,
            reasoning_content,
        })
    }

    async fn chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ProviderChatResponse> {
        let temperature = if temperature.is_none() && compatible_model_omits_temperature(model) {
            None
        } else {
            Some(temperature.unwrap_or(self.default_temperature()))
        };
        let credential = self.credential.as_deref();

        let normalized = Self::normalize_messages_for_upstream(request.messages).await?;
        let merge = self.effective_merge_system(model);
        let effective_messages = Self::flatten_system_messages(&normalized, merge);
        let effective_messages = if self.native_tool_calling {
            effective_messages
        } else {
            self.strip_native_tool_messages(&effective_messages)
        };

        // When wire_api = "responses", route all turns through the responses API.

        let tools = self.convert_tool_specs_for_model(request.tools, model);
        let native_request = self.build_native_tool_chat_request(
            &effective_messages,
            tools,
            model,
            temperature,
            !merge,
        );

        let url = self.chat_completions_url();
        let response = match self
            .apply_auth_header(
                self.http_client().post(&url).json(&native_request),
                credential,
            )
            .send()
            .await
        {
            Ok(response) => response,
            Err(chat_error) => return Err(chat_error.into()),
        };

        if !response.status().is_success() {
            let status = response.status();
            let error = response.text().await?;
            let sanitized = super::sanitize_api_error(&error);

            if Self::is_native_tool_schema_unsupported(status, &sanitized) {
                let fallback_messages =
                    Self::with_prompt_guided_tool_instructions(request.messages, request.tools);
                let text = self
                    .chat_with_history(&fallback_messages, model, temperature)
                    .await?;
                return Ok(ProviderChatResponse {
                    text: Some(text),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                });
            }

            anyhow::bail!("{} API error ({status}): {sanitized}", self.name);
        }

        let native_response: ApiChatResponse = response.json().await?;
        let usage = native_response.usage.map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cached_input_tokens: None,
        });
        let message = native_response
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message)
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"model_provider": &self.name})),
                    "compatible: empty choices in response"
                );
                anyhow::Error::msg(format!("No response from {}", self.name))
            })?;

        let mut result = self.parse_native_response(message);
        result.usage = usage;
        Ok(result)
    }

    fn supports_native_tools(&self) -> bool {
        self.native_tool_calling
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_streaming_tool_events(&self) -> bool {
        // The responses API always supports streaming tool events.
        self.native_tool_calling
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

        let temperature = if temperature.is_none() && compatible_model_omits_temperature(model) {
            None
        } else {
            Some(temperature.unwrap_or(self.default_temperature()))
        };
        let provider = self.clone();
        let messages_owned: Vec<ChatMessage> = request.messages.to_vec();
        let tools_owned: Option<Vec<zeroclaw_api::tool::ToolSpec>> =
            request.tools.map(<[zeroclaw_api::tool::ToolSpec]>::to_vec);
        let model = model.to_string();
        let count_tokens = options.count_tokens;
        let options_enabled = options.enabled;

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(100);

        let handle = ::zeroclaw_spawn::spawn!(async move {
            let normalized = match Self::normalize_messages_for_upstream(&messages_owned).await {
                Ok(n) => n,
                Err(err) => {
                    let _ = tx
                        .send(Err(StreamError::ModelProvider(err.to_string())))
                        .await;
                    return;
                }
            };

            let merge = provider.effective_merge_system(&model);
            let has_tools = tools_owned.as_ref().is_some_and(|tools| !tools.is_empty());
            let effective_messages = Self::flatten_system_messages(&normalized, merge);
            let effective_messages = provider.strip_native_tool_messages(&effective_messages);
            let tools = provider.convert_tool_specs_for_model(tools_owned.as_deref(), &model);

            let payload_result = if has_tools {
                serde_json::to_value(NativeChatRequest {
                    model: model.clone(),
                    messages: provider.convert_messages_for_native(&effective_messages, !merge),
                    temperature,
                    reasoning_effort: provider.reasoning_effort_for_model(&model),
                    tool_stream: if options_enabled {
                        provider.tool_stream_for_tools(true)
                    } else {
                        None
                    },
                    stream: Some(options_enabled),
                    // Mirror the no-tools path: opt the streaming response into a
                    // final `usage` event so `/ws/chat` can record token usage
                    // even when native tools are active.
                    stream_options: options_enabled.then_some(StreamOptionsBody {
                        include_usage: true,
                    }),
                    tools: tools.clone(),
                    tool_choice: tools.as_ref().map(|_| "auto".to_string()),
                    max_tokens: provider.max_tokens,
                })
            } else {
                let messages = effective_messages
                    .iter()
                    .map(|message| Message {
                        role: message.role.clone(),
                        content: Self::to_message_content(&message.role, &message.content, !merge),
                    })
                    .collect();

                serde_json::to_value(ApiChatRequest {
                    model: model.clone(),
                    messages,
                    temperature,
                    reasoning_effort: provider.reasoning_effort_for_model(&model),
                    tool_stream: if options_enabled {
                        provider.tool_stream_for_tools(false)
                    } else {
                        None
                    },
                    stream: Some(options_enabled),
                    stream_options: options_enabled.then_some(StreamOptionsBody {
                        include_usage: true,
                    }),
                    tools: None,
                    tool_choice: None,
                    max_tokens: provider.max_tokens,
                })
            };

            let payload = match payload_result {
                Ok(payload) => payload,
                Err(error) => {
                    let _ = tx.send(Err(StreamError::Json(error))).await;
                    return;
                }
            };

            let url = provider.chat_completions_url();
            let client = provider.streaming_http_client();
            let auth_header = provider.auth_header.clone();
            let credential = provider.credential.clone();
            let targets_mistral_tool_call_contract = provider.targets_mistral_tool_call_contract();

            let mut req_builder = client.post(&url).json(&payload);
            req_builder = apply_auth_to_request(req_builder, &auth_header, credential.as_deref());
            req_builder = req_builder.header("Accept", "text/event-stream");

            let response = match req_builder.send().await {
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
                let error = match response.text().await {
                    Ok(text) => text,
                    Err(_) => format!("HTTP error: {}", status),
                };
                let _ = tx
                    .send(Err(StreamError::ModelProvider(format!(
                        "{}: {}",
                        status, error
                    ))))
                    .await;
                return;
            }

            let mut event_stream = sse_bytes_to_events_for_contract(
                response,
                count_tokens,
                targets_mistral_tool_call_contract,
            );
            while let Some(event) = event_stream.next().await {
                if tx.send(event).await.is_err() {
                    break;
                }
            }
        });

        let guard = AbortOnDrop::new(handle.abort_handle());
        stream::unfold((rx, guard), |(mut rx, guard)| async move {
            rx.recv().await.map(|event| (event, (rx, guard)))
        })
        .boxed()
    }

    fn stream_chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
        let temperature = if temperature.is_none() && compatible_model_omits_temperature(model) {
            None
        } else {
            Some(temperature.unwrap_or(self.default_temperature()))
        };
        let provider = self.clone();
        let system_prompt_owned: Option<String> = system_prompt.map(str::to_string);
        let message_owned = message.to_string();
        let model = model.to_string();
        let count_tokens = options.count_tokens;
        let options_enabled = options.enabled;

        // Use a channel to bridge the async HTTP response to the stream
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamChunk>>(100);

        let handle = ::zeroclaw_spawn::spawn!(async move {
            // Normalize image markers in the user-supplied message before
            // forwarding upstream — see issue #6399 for the OpenAI-compatible
            // remote-vs-local file path problem.
            let user_msg = ChatMessage {
                role: "user".to_string(),
                content: message_owned,
            };
            let normalized_user = match Self::normalize_messages_for_upstream(std::slice::from_ref(
                &user_msg,
            ))
            .await
            {
                Ok(mut msgs) => msgs.pop().unwrap_or(user_msg),
                Err(err) => {
                    let _ = tx
                        .send(Err(StreamError::ModelProvider(err.to_string())))
                        .await;
                    return;
                }
            };
            let normalized_message_content = normalized_user.content;

            let merge = provider.effective_merge_system(&model);
            let mut messages = Vec::new();
            if merge {
                let content = match system_prompt_owned.as_deref() {
                    Some(sys) => format!("{sys}\n\n{normalized_message_content}"),
                    None => normalized_message_content,
                };
                messages.push(Message {
                    role: "user".to_string(),
                    content: Self::to_message_content("user", &content, !merge),
                });
            } else {
                if let Some(sys) = system_prompt_owned {
                    messages.push(Message {
                        role: "system".to_string(),
                        content: MessageContent::Text(sys),
                    });
                }
                messages.push(Message {
                    role: "user".to_string(),
                    content: Self::to_message_content("user", &normalized_message_content, !merge),
                });
            }

            let request = ApiChatRequest {
                model: model.clone(),
                messages,
                temperature,
                stream: Some(options_enabled),
                stream_options: options_enabled.then_some(StreamOptionsBody {
                    include_usage: true,
                }),
                reasoning_effort: provider.reasoning_effort_for_model(&model),
                tool_stream: None,
                tools: None,
                tool_choice: None,
                max_tokens: provider.max_tokens,
            };

            let url = provider.chat_completions_url();
            let client = provider.streaming_http_client();
            let auth_header = provider.auth_header.clone();
            let credential = provider.credential.clone();

            // Build request with auth
            let mut req_builder = client.post(&url).json(&request);

            // Apply auth header
            req_builder = apply_auth_to_request(req_builder, &auth_header, credential.as_deref());

            // Set accept header for streaming
            req_builder = req_builder.header("Accept", "text/event-stream");

            // Send request
            let response = match req_builder.send().await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx
                        .send(Err(StreamError::Http(super::format_error_chain(&e))))
                        .await;
                    return;
                }
            };

            // Check status
            if !response.status().is_success() {
                let status = response.status();
                let error = match response.text().await {
                    Ok(e) => e,
                    Err(_) => format!("HTTP error: {}", status),
                };
                let _ = tx
                    .send(Err(StreamError::ModelProvider(format!(
                        "{}: {}",
                        status, error
                    ))))
                    .await;
                return;
            }

            // Convert to chunk stream and forward to channel
            let mut chunk_stream = sse_bytes_to_chunks(response, count_tokens);
            while let Some(chunk) = chunk_stream.next().await {
                if tx.send(chunk).await.is_err() {
                    break; // Receiver dropped
                }
            }
        });

        // Convert channel receiver to stream
        let guard = AbortOnDrop::new(handle.abort_handle());
        stream::unfold((rx, guard), |(mut rx, guard)| async move {
            rx.recv().await.map(|chunk| (chunk, (rx, guard)))
        })
        .boxed()
    }

    fn stream_chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
        let temperature = if temperature.is_none() && compatible_model_omits_temperature(model) {
            None
        } else {
            Some(temperature.unwrap_or(self.default_temperature()))
        };
        let provider = self.clone();
        let messages_owned: Vec<ChatMessage> = messages.to_vec();
        let model = model.to_string();
        let count_tokens = options.count_tokens;
        let options_enabled = options.enabled;

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamChunk>>(100);

        let handle = ::zeroclaw_spawn::spawn!(async move {
            let normalized = match Self::normalize_messages_for_upstream(&messages_owned).await {
                Ok(n) => n,
                Err(err) => {
                    let _ = tx
                        .send(Err(StreamError::ModelProvider(err.to_string())))
                        .await;
                    return;
                }
            };

            let merge = provider.effective_merge_system(&model);
            let effective_messages = Self::flatten_system_messages(&normalized, merge);
            let effective_messages = provider.strip_native_tool_messages(&effective_messages);
            let api_messages: Vec<Message> = effective_messages
                .iter()
                .map(|m| Message {
                    role: m.role.clone(),
                    content: Self::to_message_content(&m.role, &m.content, !merge),
                })
                .collect();

            let request = ApiChatRequest {
                model: model.clone(),
                messages: api_messages,
                temperature,
                stream: Some(options_enabled),
                stream_options: options_enabled.then_some(StreamOptionsBody {
                    include_usage: true,
                }),
                reasoning_effort: provider.reasoning_effort_for_model(&model),
                tool_stream: None,
                tools: None,
                tool_choice: None,
                max_tokens: provider.max_tokens,
            };

            let url = provider.chat_completions_url();
            let client = provider.streaming_http_client();
            let auth_header = provider.auth_header.clone();
            let credential = provider.credential.clone();

            let mut req_builder = client.post(&url).json(&request);
            req_builder = apply_auth_to_request(req_builder, &auth_header, credential.as_deref());
            req_builder = req_builder.header("Accept", "text/event-stream");

            let response = match req_builder.send().await {
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
                let error = match response.text().await {
                    Ok(e) => e,
                    Err(_) => format!("HTTP error: {}", status),
                };
                let _ = tx
                    .send(Err(StreamError::ModelProvider(format!(
                        "{}: {}",
                        status, error
                    ))))
                    .await;
                return;
            }

            let mut chunk_stream = sse_bytes_to_chunks(response, count_tokens);
            while let Some(chunk) = chunk_stream.next().await {
                if tx.send(chunk).await.is_err() {
                    break;
                }
            }
        });

        let guard = AbortOnDrop::new(handle.abort_handle());
        stream::unfold((rx, guard), |(mut rx, guard)| async move {
            rx.recv().await.map(|chunk| (chunk, (rx, guard)))
        })
        .boxed()
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        // Hit the appropriate URL with a GET to prime the connection pool.
        // The server will likely return 405 Method Not Allowed, which is fine.
        let url = self.chat_completions_url();
        let _ = self
            .apply_auth_header(self.http_client().get(&url), self.credential.as_deref())
            .send()
            .await?;
        Ok(())
    }
}

impl ::zeroclaw_api::attribution::Attributable for OpenAiCompatibleModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Plugin,
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

    fn make_model_provider(
        name: &str,
        url: &str,
        key: Option<&str>,
    ) -> OpenAiCompatibleModelProvider {
        OpenAiCompatibleModelProvider::new("test", name, url, key, AuthStyle::Bearer)
    }

    #[test]
    fn creates_with_key() {
        let p = make_model_provider(
            "venice",
            "https://api.venice.ai",
            Some("venice-test-credential"),
        );
        assert_eq!(p.name, "venice");
        assert_eq!(p.base_url, "https://api.venice.ai");
        assert_eq!(p.credential.as_deref(), Some("venice-test-credential"));
    }

    #[test]
    fn creates_without_key() {
        let p = make_model_provider("test", "https://example.com", None);
        assert!(p.credential.is_none());
    }

    #[test]
    fn strips_trailing_slash() {
        let p = make_model_provider("test", "https://example.com/", None);
        assert_eq!(p.base_url, "https://example.com");
    }

    #[tokio::test]
    async fn chat_without_key_attempts_request() {
        let p = make_model_provider("Local", "http://127.0.0.1:1", None);
        let result = p
            .chat_with_system(None, "hello", "default", Some(0.7))
            .await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            !err_msg.contains("API key not set"),
            "should not get credential error, got: {err_msg}"
        );
    }

    #[test]
    fn native_chat_request_with_tools_includes_stream_options() {
        // Regression: tool-enabled streaming requests must opt the response
        // into a final `usage` SSE event, otherwise OpenAI-compatible providers
        // never report token counts on the `/ws/chat` path (the gateway's
        // primary path uses native tools). See Audacity88's #6159 review.
        let req = NativeChatRequest {
            model: "gpt-4o".to_string(),
            messages: vec![NativeMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("hello".to_string())),
                tool_call_id: None,
                tool_calls: None,
                reasoning_content: None,
            }],
            temperature: Some(0.7),
            stream: Some(true),
            stream_options: Some(StreamOptionsBody {
                include_usage: true,
            }),
            reasoning_effort: None,
            tool_stream: None,
            tools: Some(vec![serde_json::json!({"name": "echo"})]),
            tool_choice: Some("auto".to_string()),
            max_tokens: None,
        };
        let value: serde_json::Value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value
                .get("stream_options")
                .and_then(|v| v.get("include_usage"))
                .and_then(serde_json::Value::as_bool),
            Some(true),
            "tool-enabled streaming request must serialize stream_options.include_usage=true; \
             without it OpenAI-compatible providers omit the final usage event"
        );
    }

    #[test]
    fn native_chat_request_omits_stream_options_when_none() {
        // Non-streaming path (e.g. classic `chat()` call) does not need
        // `stream_options.include_usage` because the final response carries
        // `usage` directly. The field must be skipped in serialization.
        let req = NativeChatRequest {
            model: "gpt-4o".to_string(),
            messages: vec![],
            temperature: Some(0.7),
            stream: Some(false),
            stream_options: None,
            reasoning_effort: None,
            tool_stream: None,
            tools: None,
            tool_choice: None,
            max_tokens: None,
        };
        let value: serde_json::Value = serde_json::to_value(&req).unwrap();
        assert!(
            value.get("stream_options").is_none(),
            "non-streaming NativeChatRequest must not emit a stream_options key"
        );
    }

    #[test]
    fn normalize_model_ids_trims_filters_and_sorts() {
        let body = serde_json::from_value(serde_json::json!({
            "data": [
                {"id": " zeta-model "},
                {"id": ""},
                {"id": "alpha-model"}
            ]
        }))
        .unwrap();

        assert_eq!(normalize_model_ids(body), vec!["alpha-model", "zeta-model"]);
    }

    #[test]
    fn request_serializes_correctly() {
        let req = ApiChatRequest {
            model: "llama-3.3-70b".to_string(),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: MessageContent::Text("You are ZeroClaw".to_string()),
                },
                Message {
                    role: "user".to_string(),
                    content: MessageContent::Text("hello".to_string()),
                },
            ],
            temperature: Some(0.4),
            stream: Some(false),
            stream_options: None,
            reasoning_effort: None,
            tool_stream: None,
            tools: None,
            tool_choice: None,
            max_tokens: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("llama-3.3-70b"));
        assert!(json.contains("system"));
        assert!(json.contains("user"));
        // tools/tool_choice should be omitted when None
        assert!(!json.contains("tools"));
        assert!(!json.contains("tool_choice"));
    }

    #[test]
    fn response_deserializes() {
        let json = r#"{"choices":[{"message":{"content":"Hello from Venice!"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            resp.choices[0].message.content,
            Some("Hello from Venice!".to_string())
        );
    }

    #[test]
    fn response_deserializes_content_as_openai_text_parts_array() {
        let json =
            r#"{"choices":[{"message":{"content":[{"type":"text","text":"Hello array"}]}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            resp.choices[0].message.content.as_deref(),
            Some("Hello array")
        );
    }

    #[test]
    fn response_deserializes_multiple_text_parts_with_newlines() {
        let json = r#"{"choices":[{"message":{"content":[{"type":"text","text":"Hello"},{"type":"image_url","image_url":{"url":"https://example.com/image.png"}},{"type":"text","text":"array"}]}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            resp.choices[0].message.content.as_deref(),
            Some("Hello\narray")
        );
    }

    #[test]
    fn response_rejects_unsupported_top_level_content_shape() {
        let json = r#"{"choices":[{"message":{"content":{"type":"text","text":"Hello object"}}}]}"#;
        serde_json::from_str::<ApiChatResponse>(json)
            .expect_err("object-shaped assistant content must remain an invalid payload");
    }

    #[test]
    fn response_empty_choices() {
        let json = r#"{"choices":[]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.choices.is_empty());
    }

    #[test]
    fn parse_chat_response_body_reports_sanitized_snippet() {
        let body = r#"{"choices":"invalid","api_key":"sk-test-secret-value"}"#;
        let err = parse_chat_response_body("custom", body).expect_err("payload should fail");
        let msg = err.to_string();

        assert!(msg.contains("custom API returned an unexpected chat-completions payload"));
        assert!(msg.contains("body="));
        assert!(msg.contains("[REDACTED]"));
        assert!(!msg.contains("sk-test-secret-value"));
    }

    #[test]
    fn x_api_key_auth_style() {
        let p = OpenAiCompatibleModelProvider::new(
            "test",
            "moonshot",
            "https://api.moonshot.cn",
            Some("ms-key"),
            AuthStyle::XApiKey,
        );
        assert!(matches!(p.auth_header, AuthStyle::XApiKey));
    }

    #[test]
    fn custom_auth_style() {
        let p = OpenAiCompatibleModelProvider::new(
            "test",
            "custom",
            "https://api.example.com",
            Some("key"),
            AuthStyle::Custom("X-Custom-Key".into()),
        );
        assert!(matches!(p.auth_header, AuthStyle::Custom(_)));
    }

    #[test]
    fn zhipu_jwt_produces_valid_three_part_token() {
        let result = zhipu_jwt_bearer("testid.testsecret").unwrap();
        assert!(result.starts_with("Bearer "));
        let jwt = result.strip_prefix("Bearer ").unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT must have 3 dot-separated parts: {jwt}");
    }

    #[test]
    fn zhipu_jwt_header_is_correct() {
        use base64::engine::{Engine, general_purpose::URL_SAFE_NO_PAD};
        let result = zhipu_jwt_bearer("myid.mysecret").unwrap();
        let jwt = result.strip_prefix("Bearer ").unwrap();
        let header_b64 = jwt.split('.').next().unwrap();
        let header_bytes = URL_SAFE_NO_PAD.decode(header_b64).unwrap();
        let header: serde_json::Value = serde_json::from_slice(&header_bytes).unwrap();
        assert_eq!(header["alg"], "HS256");
        assert_eq!(header["typ"], "JWT");
        assert_eq!(header["sign_type"], "SIGN");
    }

    #[test]
    fn zhipu_jwt_payload_contains_api_key_and_timestamps() {
        use base64::engine::{Engine, general_purpose::URL_SAFE_NO_PAD};
        let result = zhipu_jwt_bearer("myapiid.mysecretkey").unwrap();
        let jwt = result.strip_prefix("Bearer ").unwrap();
        let payload_b64 = jwt.split('.').nth(1).unwrap();
        let payload_bytes = URL_SAFE_NO_PAD.decode(payload_b64).unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).unwrap();
        assert_eq!(payload["api_key"], "myapiid");
        assert!(payload["exp"].is_number());
        assert!(payload["timestamp"].is_number());
        // exp should be ~210s after timestamp
        let ts = payload["timestamp"].as_u64().unwrap();
        let exp = payload["exp"].as_u64().unwrap();
        assert_eq!(exp - ts, 210_000);
    }

    #[test]
    fn zhipu_jwt_signature_is_verifiable() {
        let secret = "testsecret123";
        let credential = format!("testid.{secret}");
        let result = zhipu_jwt_bearer(&credential).unwrap();
        let jwt = result.strip_prefix("Bearer ").unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        let signing_input = format!("{}.{}", parts[0], parts[1]);

        // Verify HMAC-SHA256 signature
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
        use base64::engine::{Engine, general_purpose::URL_SAFE_NO_PAD};
        let sig_bytes = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        ring::hmac::verify(&key, signing_input.as_bytes(), &sig_bytes)
            .expect("signature must verify");
    }

    #[test]
    fn zhipu_jwt_rejects_invalid_key_format() {
        assert!(zhipu_jwt_bearer("no-dot-here").is_err());
        assert!(zhipu_jwt_bearer("").is_err());
    }

    #[test]
    fn zhipu_jwt_auth_style_applies_correctly() {
        let p = OpenAiCompatibleModelProvider::new(
            "test",
            "Z.AI",
            "https://api.z.ai/api/coding/paas/v4",
            Some("testid.testsecret"),
            AuthStyle::ZhipuJwt,
        );
        assert!(matches!(p.auth_header, AuthStyle::ZhipuJwt));
    }

    #[tokio::test]
    async fn all_compatible_providers_attempt_request_without_key() {
        let model_providers = vec![
            make_model_provider("Venice", "http://127.0.0.1:1", None),
            make_model_provider("Moonshot", "http://127.0.0.1:1", None),
            make_model_provider("GLM", "http://127.0.0.1:1", None),
            make_model_provider("MiniMax", "http://127.0.0.1:1", None),
            make_model_provider("Groq", "http://127.0.0.1:1", None),
            make_model_provider("Mistral", "http://127.0.0.1:1", None),
            make_model_provider("xAI", "http://127.0.0.1:1", None),
            make_model_provider("Astrai", "http://127.0.0.1:1", None),
        ];

        for p in model_providers {
            let result = p.chat_with_system(None, "test", "model", Some(0.7)).await;
            assert!(result.is_err(), "{} should fail (unreachable host)", p.name);
            let err_msg = result.unwrap_err().to_string();
            assert!(
                !err_msg.contains("API key not set"),
                "{} should get transport error, not credential error, got: {err_msg}",
                p.name
            );
        }
    }

    #[test]
    fn tool_call_function_name_falls_back_to_top_level_name() {
        let call: ToolCall = serde_json::from_value(serde_json::json!({
            "name": "memory_recall",
            "arguments": "{\"query\":\"latest roadmap\"}"
        }))
        .unwrap();

        assert_eq!(call.function_name().as_deref(), Some("memory_recall"));
    }

    #[test]
    fn tool_call_function_arguments_falls_back_to_parameters_object() {
        let call: ToolCall = serde_json::from_value(serde_json::json!({
            "name": "shell",
            "parameters": {"command": "pwd"}
        }))
        .unwrap();

        assert_eq!(
            call.function_arguments().as_deref(),
            Some("{\"command\":\"pwd\"}")
        );
    }

    #[test]
    fn tool_call_function_arguments_prefers_nested_function_field() {
        let call: ToolCall = serde_json::from_value(serde_json::json!({
            "name": "ignored_name",
            "arguments": "{\"query\":\"ignored\"}",
            "function": {
                "name": "memory_recall",
                "arguments": "{\"query\":\"preferred\"}"
            }
        }))
        .unwrap();

        assert_eq!(call.function_name().as_deref(), Some("memory_recall"));
        assert_eq!(
            call.function_arguments().as_deref(),
            Some("{\"query\":\"preferred\"}")
        );
    }

    // ----------------------------------------------------------
    // Custom endpoint path tests (Issue #114)
    // ----------------------------------------------------------

    #[test]
    fn chat_completions_url_standard_openai() {
        // Standard OpenAI-compatible model_providers get /chat/completions appended
        let p = make_model_provider("openai", "https://api.openai.com/v1", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_trailing_slash() {
        // Trailing slash is stripped, then /chat/completions appended
        let p = make_model_provider("test", "https://api.example.com/v1/", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_volcengine_ark() {
        // VolcEngine ARK uses custom path - should use as-is
        let p = make_model_provider(
            "volcengine",
            "https://ark.cn-beijing.volces.com/api/coding/v3/chat/completions",
            None,
        );
        assert_eq!(
            p.chat_completions_url(),
            "https://ark.cn-beijing.volces.com/api/coding/v3/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_custom_full_endpoint() {
        // Custom model_provider with full endpoint path
        let p = make_model_provider(
            "custom",
            "https://my-api.example.com/v2/llm/chat/completions",
            None,
        );
        assert_eq!(
            p.chat_completions_url(),
            "https://my-api.example.com/v2/llm/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_requires_exact_suffix_match() {
        let p = make_model_provider(
            "custom",
            "https://my-api.example.com/v2/llm/chat/completions-proxy",
            None,
        );
        assert_eq!(
            p.chat_completions_url(),
            "https://my-api.example.com/v2/llm/chat/completions-proxy/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_without_v1() {
        // ModelProvider configured without /v1 in base URL
        let p = make_model_provider("test", "https://api.example.com", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://api.example.com/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_base_with_v1() {
        // ModelProvider configured with /v1 in base URL
        let p = make_model_provider("test", "https://api.example.com/v1", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://api.example.com/v1/chat/completions"
        );
    }

    // ----------------------------------------------------------
    // ModelProvider-specific endpoint tests (Issue #167)
    // ----------------------------------------------------------

    #[test]
    fn chat_completions_url_zai() {
        // Z.AI uses /api/paas/v4 base path
        let p = make_model_provider("zai", "https://api.z.ai/api/paas/v4", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://api.z.ai/api/paas/v4/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_minimax() {
        // MiniMax OpenAI-compatible endpoint requires /v1 base path.
        let p = make_model_provider("minimax", "https://api.minimaxi.com/v1", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://api.minimaxi.com/v1/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_glm() {
        // GLM (BigModel) uses /api/paas/v4 base path
        let p = make_model_provider("glm", "https://open.bigmodel.cn/api/paas/v4", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://open.bigmodel.cn/api/paas/v4/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_opencode() {
        // OpenCode Zen uses /zen/v1 base path
        let p = make_model_provider("opencode", "https://opencode.ai/zen/v1", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://opencode.ai/zen/v1/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_opencode_go() {
        // OpenCode Go uses /zen/go/v1 base path
        let p = make_model_provider("opencode-go", "https://opencode.ai/zen/go/v1", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://opencode.ai/zen/go/v1/chat/completions"
        );
    }

    #[test]
    fn parse_native_response_preserves_tool_call_id() {
        let provider = make_model_provider("test", "https://example.com", None);
        let message = ResponseMessage {
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: Some("call_123".to_string()),
                kind: Some("function".to_string()),
                function: Some(Function {
                    name: Some("shell".to_string()),
                    arguments: Some(r#"{"command":"pwd"}"#.to_string()),
                }),
                name: None,
                arguments: None,
                parameters: None,
                extra_content: None,
            }]),
            reasoning_content: None,
        };

        let parsed = provider.parse_native_response(message);
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].id, "call_123");
        assert_eq!(parsed.tool_calls[0].name, "shell");
    }

    #[test]
    fn parse_native_response_mistral_normalizes_invalid_tool_call_id() {
        let provider = make_model_provider("Mistral", "https://api.mistral.ai/v1", None);
        let message = ResponseMessage {
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: Some("xvL0p9bZ41j2X0O3Q1y9vL0p9bZ41j2X".to_string()),
                kind: Some("function".to_string()),
                function: Some(Function {
                    name: Some("shell".to_string()),
                    arguments: Some(r#"{"command":"pwd"}"#.to_string()),
                }),
                name: None,
                arguments: None,
                parameters: None,
                extra_content: None,
            }]),
            reasoning_content: None,
        };

        let parsed = provider.parse_native_response(message);
        assert_eq!(parsed.tool_calls.len(), 1);
        let id = &parsed.tool_calls[0].id;
        assert_eq!(id.len(), 9);
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn parse_native_response_mistral_generates_valid_id_when_missing() {
        let provider = make_model_provider("Mistral", "https://api.mistral.ai/v1", None);
        let message = ResponseMessage {
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: None,
                kind: Some("function".to_string()),
                function: Some(Function {
                    name: Some("shell".to_string()),
                    arguments: Some(r#"{"command":"pwd"}"#.to_string()),
                }),
                name: None,
                arguments: None,
                parameters: None,
                extra_content: None,
            }]),
            reasoning_content: None,
        };

        let parsed = provider.parse_native_response(message);
        assert_eq!(parsed.tool_calls.len(), 1);
        let id = &parsed.tool_calls[0].id;
        assert_eq!(id.len(), 9);
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn parse_native_response_custom_mistral_endpoint_normalizes_tool_call_id() {
        let provider = make_model_provider("Custom", "https://api.mistral.ai/v1", None);
        let message = ResponseMessage {
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: Some("xvL0p9bZ41j2X0O3Q1y9vL0p9bZ41j2X".to_string()),
                kind: Some("function".to_string()),
                function: Some(Function {
                    name: Some("shell".to_string()),
                    arguments: Some(r#"{"command":"pwd"}"#.to_string()),
                }),
                name: None,
                arguments: None,
                parameters: None,
                extra_content: None,
            }]),
            reasoning_content: None,
        };

        let parsed = provider.parse_native_response(message);
        assert_eq!(parsed.tool_calls.len(), 1);
        let id = &parsed.tool_calls[0].id;
        assert_eq!(id.len(), 9);
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn parse_native_response_mistral_avoids_id_collision_after_normalization() {
        let provider = make_model_provider("Mistral", "https://api.mistral.ai/v1", None);
        let message = ResponseMessage {
            content: None,
            tool_calls: Some(vec![
                ToolCall {
                    id: Some("ABCDEFGHI123".to_string()),
                    kind: Some("function".to_string()),
                    function: Some(Function {
                        name: Some("shell".to_string()),
                        arguments: Some(r#"{"command":"pwd"}"#.to_string()),
                    }),
                    name: None,
                    arguments: None,
                    parameters: None,
                    extra_content: None,
                },
                ToolCall {
                    id: Some("ABCDEFGHIxyz".to_string()),
                    kind: Some("function".to_string()),
                    function: Some(Function {
                        name: Some("echo".to_string()),
                        arguments: Some(r#"{"text":"ok"}"#.to_string()),
                    }),
                    name: None,
                    arguments: None,
                    parameters: None,
                    extra_content: None,
                },
            ]),
            reasoning_content: None,
        };

        let parsed = provider.parse_native_response(message);
        assert_eq!(parsed.tool_calls.len(), 2);
        let id0 = &parsed.tool_calls[0].id;
        let id1 = &parsed.tool_calls[1].id;
        assert_eq!(id0.len(), 9);
        assert_eq!(id1.len(), 9);
        assert!(id0.chars().all(|c| c.is_ascii_alphanumeric()));
        assert!(id1.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(id0, id1);
    }

    #[test]
    fn convert_messages_for_native_maps_tool_result_payload() {
        let input = vec![ChatMessage::tool(
            r#"{"tool_call_id":"call_abc","content":"done"}"#,
        )];

        let provider = make_model_provider("test", "https://example.com", None);
        let converted = provider.convert_messages_for_native(&input, true);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].role, "tool");
        assert_eq!(converted[0].tool_call_id.as_deref(), Some("call_abc"));
        assert!(matches!(
            converted[0].content.as_ref(),
            Some(MessageContent::Text(value)) if value == "done"
        ));
    }

    #[test]
    fn native_chat_request_mistral_serializes_matching_valid_tool_call_ids() {
        let provider = make_model_provider("Mistral", "https://api.mistral.ai/v1", None);
        let invalid_id = "chatcmpl-tool-abc";
        let history_json = serde_json::json!({
            "content": "",
            "tool_calls": [{
                "id": invalid_id,
                "name": "shell",
                "arguments": "{\"cmd\":\"pwd\"}"
            }]
        });
        let messages = vec![
            ChatMessage::assistant(history_json.to_string()),
            ChatMessage::tool(
                serde_json::json!({
                    "tool_call_id": invalid_id,
                    "content": "done"
                })
                .to_string(),
            ),
        ];

        let req = NativeChatRequest {
            model: "mistral-large-latest".to_string(),
            messages: provider.convert_messages_for_native(&messages, true),
            temperature: Some(0.7),
            stream: Some(false),
            stream_options: None,
            reasoning_effort: None,
            tool_stream: None,
            tools: Some(vec![serde_json::json!({
                "type": "function",
                "function": {
                    "name": "shell",
                    "description": "Run a shell command",
                    "parameters": {"type": "object"}
                }
            })]),
            tool_choice: Some("auto".to_string()),
            max_tokens: None,
        };

        let value = serde_json::to_value(&req).unwrap();
        let assistant_id = value["messages"][0]["tool_calls"][0]["id"]
            .as_str()
            .expect("assistant tool call id should serialize");
        let tool_id = value["messages"][1]["tool_call_id"]
            .as_str()
            .expect("tool result id should serialize");

        assert_ne!(assistant_id, invalid_id);
        assert!(is_valid_mistral_tool_call_id(assistant_id));
        assert_eq!(assistant_id, tool_id);
    }

    #[test]
    fn convert_messages_for_native_keeps_user_image_markers_as_text_when_disabled() {
        let input = vec![ChatMessage::user(
            "System primer [IMAGE:data:image/png;base64,abcd] user turn",
        )];

        let provider = make_model_provider("test", "https://example.com", None);
        let converted = provider.convert_messages_for_native(&input, false);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].role, "user");
        assert!(matches!(
            converted[0].content.as_ref(),
            Some(MessageContent::Text(value))
                if value == "System primer [IMAGE:data:image/png;base64,abcd] user turn"
        ));
    }

    #[test]
    fn flatten_system_messages_merges_into_first_user() {
        let input = vec![
            ChatMessage::system("core policy"),
            ChatMessage::assistant("ack"),
            ChatMessage::system("delivery rules"),
            ChatMessage::user("hello"),
            ChatMessage::assistant("post-user"),
        ];

        let output = OpenAiCompatibleModelProvider::flatten_system_messages(&input, true);
        assert_eq!(output.len(), 3);
        assert_eq!(output[0].role, "assistant");
        assert_eq!(output[0].content, "ack");
        assert_eq!(output[1].role, "user");
        assert_eq!(output[1].content, "core policy\n\ndelivery rules\n\nhello");
        assert_eq!(output[2].role, "assistant");
        assert_eq!(output[2].content, "post-user");
        assert!(output.iter().all(|m| m.role != "system"));
    }

    #[test]
    fn flatten_system_messages_inserts_user_when_missing() {
        let input = vec![
            ChatMessage::system("core policy"),
            ChatMessage::assistant("ack"),
        ];

        let output = OpenAiCompatibleModelProvider::flatten_system_messages(&input, true);
        assert_eq!(output.len(), 2);
        assert_eq!(output[0].role, "user");
        assert_eq!(output[0].content, "core policy");
        assert_eq!(output[1].role, "assistant");
        assert_eq!(output[1].content, "ack");
    }

    #[test]
    fn strip_think_tags_drops_unclosed_block_suffix() {
        let input = "visible<think>hidden";
        assert_eq!(strip_think_tags(input), "visible");
    }

    #[test]
    fn native_tool_schema_unsupported_detection_is_precise() {
        assert!(
            OpenAiCompatibleModelProvider::is_native_tool_schema_unsupported(
                reqwest::StatusCode::BAD_REQUEST,
                "unknown parameter: tools"
            )
        );
        assert!(
            !OpenAiCompatibleModelProvider::is_native_tool_schema_unsupported(
                reqwest::StatusCode::UNAUTHORIZED,
                "unknown parameter: tools"
            )
        );
    }

    #[test]
    fn native_tool_schema_unsupported_detects_groq_tool_validation_error() {
        assert!(
            OpenAiCompatibleModelProvider::is_native_tool_schema_unsupported(
                reqwest::StatusCode::BAD_REQUEST,
                r#"Groq API error (400 Bad Request): {"error":{"message":"tool call validation failed: attempted to call tool 'memory_recall={\"limit\":5}' which was not in request"}}"#
            )
        );
    }

    #[test]
    fn prompt_guided_tool_fallback_injects_system_instruction() {
        let input = vec![ChatMessage::user("check status")];
        let tools = vec![zeroclaw_api::tool::ToolSpec {
            name: "shell_exec".to_string(),
            description: "Execute shell command".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" }
                },
                "required": ["command"]
            }),
        }];

        let output = OpenAiCompatibleModelProvider::with_prompt_guided_tool_instructions(
            &input,
            Some(&tools),
        );
        assert!(!output.is_empty());
        assert_eq!(output[0].role, "system");
        assert!(output[0].content.contains("Available Tools"));
        assert!(output[0].content.contains("shell_exec"));
    }

    #[test]
    fn reasoning_effort_only_applies_to_openai_and_selected_codex_models() {
        let model_provider = make_model_provider("test", "https://example.com", None)
            .with_reasoning_effort(Some("high".to_string()));

        assert_eq!(
            model_provider.reasoning_effort_for_model("o1-preview"),
            Some("high".to_string())
        );
        assert_eq!(
            model_provider.reasoning_effort_for_model("openai/o3-mini"),
            Some("high".to_string())
        );
        assert_eq!(
            model_provider.reasoning_effort_for_model("o4-mini"),
            Some("high".to_string())
        );
        assert_eq!(
            model_provider.reasoning_effort_for_model("gpt-5"),
            Some("high".to_string())
        );
        assert_eq!(
            model_provider.reasoning_effort_for_model("gpt-5.3-codex"),
            Some("high".to_string())
        );
        assert_eq!(
            model_provider.reasoning_effort_for_model("openai/gpt-5"),
            Some("high".to_string())
        );
        assert_eq!(
            model_provider.reasoning_effort_for_model("gpt-4-codex"),
            Some("high".to_string())
        );
        assert_eq!(
            model_provider.reasoning_effort_for_model("llama-3-codex"),
            None,
            "generic codex-like model names must not receive OpenAI-only reasoning_effort",
        );
        assert_eq!(
            model_provider.reasoning_effort_for_model("llama-3.3-70b"),
            None
        );
    }

    #[tokio::test]
    async fn warmup_without_key_attempts_connection() {
        let model_provider = make_model_provider("test", "http://127.0.0.1:1", None);
        let result = model_provider.warmup().await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            !err_msg.contains("API key not set"),
            "should not get credential error, got: {err_msg}"
        );
    }

    // ══════════════════════════════════════════════════════════
    // Native tool calling tests
    // ══════════════════════════════════════════════════════════

    #[test]
    fn capabilities_reports_native_tool_calling() {
        let p = make_model_provider("test", "https://example.com", None);
        let caps = <OpenAiCompatibleModelProvider as ModelProvider>::capabilities(&p);
        assert!(caps.native_tool_calling);
        assert!(!caps.vision);
    }

    #[test]
    fn capabilities_reports_vision_for_qwen_compatible_provider() {
        let p = OpenAiCompatibleModelProvider::new_with_vision(
            "test",
            "Qwen",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            Some("k"),
            AuthStyle::Bearer,
            true,
        );
        let caps = <OpenAiCompatibleModelProvider as ModelProvider>::capabilities(&p);
        assert!(caps.native_tool_calling);
        assert!(caps.vision);
    }

    #[test]
    fn minimax_provider_supports_native_tool_calling_with_system_merge() {
        let p = OpenAiCompatibleModelProvider::new(
            "test",
            "MiniMax",
            "https://api.minimax.chat/v1",
            Some("k"),
            AuthStyle::Bearer,
        )
        .with_merge_system_into_user();
        let caps = <OpenAiCompatibleModelProvider as ModelProvider>::capabilities(&p);
        assert!(
            caps.native_tool_calling,
            "MiniMax should preserve native tool calling when system messages are merged"
        );
        assert!(!caps.vision);
    }

    /// Regression test for #5743: native tool messages must be stripped for
    /// model_providers that don't support native tool calling (e.g. MiniMax).
    #[test]
    fn strip_native_tool_messages_removes_tool_and_tool_calls() {
        let messages = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("search for cats"),
            ChatMessage::assistant(
                r#"{"content":"I'll search","tool_calls":[{"id":"chatcmpl-tool-abc","name":"web_search","arguments":"{}"}]}"#,
            ),
            ChatMessage::tool(
                r#"{"tool_call_id":"chatcmpl-tool-abc","content":"Found 10 results"}"#,
            ),
            ChatMessage::assistant("Here are the results about cats"),
            ChatMessage::user("thanks"),
        ];
        let p = OpenAiCompatibleModelProvider::new_merge_system_into_user(
            "test",
            "MiniMax",
            "https://api.minimax.chat/v1",
            Some("k"),
            AuthStyle::Bearer,
        );
        let stripped = p.strip_native_tool_messages(&messages);
        // tool message dropped; the pre-tool narration and the reply that
        // follows the tool result are now coalesced into a single assistant
        // message so the output never contains consecutive assistants (see
        // #5825).
        assert_eq!(stripped.len(), 4);
        assert_eq!(stripped[0].role, "system");
        assert_eq!(stripped[1].role, "user");
        assert_eq!(stripped[1].content, "search for cats");
        assert_eq!(stripped[2].role, "assistant");
        assert!(
            stripped[2].content.starts_with("I'll search"),
            "coalesced assistant must preserve the pre-tool narration; got {:?}",
            stripped[2].content
        );
        assert!(
            stripped[2]
                .content
                .contains("Here are the results about cats"),
            "coalesced assistant must preserve the post-tool reply; got {:?}",
            stripped[2].content
        );
        assert!(
            !stripped[2].content.contains("tool_calls"),
            "tool_calls structure must be stripped"
        );
        assert_eq!(stripped[3].role, "user");
    }

    #[test]
    fn strip_native_tool_messages_drops_empty_assistant_tool_calls() {
        let messages = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("do it"),
            ChatMessage::assistant(
                r#"{"content":"","tool_calls":[{"id":"tc1","name":"shell","arguments":"{}"}]}"#,
            ),
            ChatMessage::tool(r#"{"tool_call_id":"tc1","content":"ok"}"#),
            ChatMessage::assistant("Done"),
        ];
        let p = OpenAiCompatibleModelProvider::new_merge_system_into_user(
            "test",
            "MiniMax",
            "https://api.minimax.chat/v1",
            Some("k"),
            AuthStyle::Bearer,
        );
        let stripped = p.strip_native_tool_messages(&messages);
        // assistant with empty content + tool_calls → dropped; tool → dropped
        assert_eq!(stripped.len(), 3);
        assert_eq!(stripped[0].role, "system");
        assert_eq!(stripped[1].role, "user");
        assert_eq!(stripped[2].role, "assistant");
        assert_eq!(stripped[2].content, "Done");
    }

    #[test]
    fn strip_native_tool_messages_preserves_regular_messages() {
        let messages = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi there"),
            ChatMessage::user("bye"),
        ];
        let p = OpenAiCompatibleModelProvider::new_merge_system_into_user(
            "test",
            "MiniMax",
            "https://api.minimax.chat/v1",
            Some("k"),
            AuthStyle::Bearer,
        );
        let stripped = p.strip_native_tool_messages(&messages);
        assert_eq!(stripped.len(), 4);
        for (orig, result) in messages.iter().zip(stripped.iter()) {
            assert_eq!(orig.role, result.role);
            assert_eq!(orig.content, result.content);
        }
    }

    /// Confirm that `strip_native_tool_messages` is a no-op when the model_provider
    /// has `native_tool_calling = true` — tool-role and assistant-with-tool-calls
    /// messages must pass through unchanged.
    #[test]
    fn strip_native_tool_messages_passthrough_when_native_tool_calling_enabled() {
        let messages = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("search for cats"),
            ChatMessage::assistant(
                r#"{"content":"I'll search","tool_calls":[{"id":"chatcmpl-tool-abc","name":"web_search","arguments":"{}"}]}"#,
            ),
            ChatMessage::tool(
                r#"{"tool_call_id":"chatcmpl-tool-abc","content":"Found 10 results"}"#,
            ),
            ChatMessage::assistant("Here are the results about cats"),
        ];
        let p = OpenAiCompatibleModelProvider::new(
            "test",
            "NativeToolProvider",
            "https://api.example.com/v1",
            Some("k"),
            AuthStyle::Bearer,
        );
        assert!(
            <OpenAiCompatibleModelProvider as ModelProvider>::capabilities(&p).native_tool_calling,
            "model_provider must have native_tool_calling enabled for this test"
        );
        let result = p.strip_native_tool_messages(&messages);
        assert_eq!(result.len(), messages.len());
        for (orig, out) in messages.iter().zip(result.iter()) {
            assert_eq!(orig.role, out.role);
            assert_eq!(orig.content, out.content);
        }
    }

    #[test]
    fn user_agent_constructor_keeps_native_tool_calling_enabled() {
        let p = OpenAiCompatibleModelProvider::new_with_user_agent(
            "test",
            "TestProvider",
            "https://example.com",
            Some("k"),
            AuthStyle::Bearer,
            "zeroclaw-test/1.0",
        );
        let caps = <OpenAiCompatibleModelProvider as ModelProvider>::capabilities(&p);
        assert!(caps.native_tool_calling);
        assert!(!caps.vision);
        assert_eq!(p.user_agent.as_deref(), Some("zeroclaw-test/1.0"));
    }

    #[test]
    fn user_agent_and_vision_constructor_preserves_capability_flags() {
        let p = OpenAiCompatibleModelProvider::new_with_user_agent_and_vision(
            "test",
            "VisionModelProvider",
            "https://example.com",
            Some("k"),
            AuthStyle::Bearer,
            "zeroclaw-test/vision",
            true,
        );
        let caps = <OpenAiCompatibleModelProvider as ModelProvider>::capabilities(&p);
        assert!(caps.native_tool_calling);
        assert!(caps.vision);
        assert_eq!(p.user_agent.as_deref(), Some("zeroclaw-test/vision"));
    }

    #[test]
    fn to_message_content_converts_image_markers_to_openai_parts() {
        let content = "Describe this\n\n[IMAGE:data:image/png;base64,abcd]";
        let value = serde_json::to_value(OpenAiCompatibleModelProvider::to_message_content(
            "user", content, true,
        ))
        .unwrap();
        let parts = value
            .as_array()
            .expect("multimodal content should be an array");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "Describe this");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,abcd");
    }

    #[test]
    fn to_message_content_keeps_markers_as_text_when_user_image_parts_disabled() {
        let content = "Policy [IMAGE:data:image/png;base64,abcd]";
        let value = serde_json::to_value(OpenAiCompatibleModelProvider::to_message_content(
            "user", content, false,
        ))
        .unwrap();
        assert_eq!(value, serde_json::json!(content));
    }

    #[test]
    fn to_message_content_keeps_plain_text_for_non_user_roles() {
        let value = serde_json::to_value(OpenAiCompatibleModelProvider::to_message_content(
            "system",
            "You are a helpful assistant.",
            true,
        ))
        .unwrap();
        assert_eq!(value, serde_json::json!("You are a helpful assistant."));
    }

    #[tokio::test]
    async fn normalize_messages_for_upstream_rewrites_local_image_path_to_data_uri() {
        // Regression for #6399: bare local paths inside `[IMAGE:...]` markers
        // must be base64-encoded at the provider boundary so strict upstreams
        // (vLLM 0.20+) never see `image_url.url = "/home/.../photo.png"`.
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("pixel.png");
        // 1x1 transparent PNG.
        let png: [u8; 67] = [
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        std::fs::write(&path, png).expect("write pixel.png");
        let path_str = path.to_string_lossy().into_owned();

        let msg = ChatMessage {
            role: "user".into(),
            content: format!("Caption please [IMAGE:{}]", path_str),
        };

        let normalized = OpenAiCompatibleModelProvider::normalize_messages_for_upstream(
            std::slice::from_ref(&msg),
        )
        .await
        .expect("normalize ok");

        assert_eq!(normalized.len(), 1);
        let content = &normalized[0].content;
        assert!(
            content.contains("[IMAGE:data:image/png;base64,"),
            "expected base64 data URI in normalized content, got: {content}"
        );
        assert!(
            !content.contains(&path_str),
            "raw local path must not leak to upstream, got: {content}"
        );
    }

    #[test]
    fn request_serializes_with_tools() {
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get weather for a location",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "location": {"type": "string"}
                    }
                }
            }
        })];

        let req = ApiChatRequest {
            model: "test-model".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: MessageContent::Text("What is the weather?".to_string()),
            }],
            temperature: Some(0.7),
            stream: Some(false),
            stream_options: None,
            reasoning_effort: None,
            tool_stream: None,
            tools: Some(tools),
            tool_choice: Some("auto".to_string()),
            max_tokens: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"tools\""));
        assert!(json.contains("get_weather"));
        assert!(json.contains("\"tool_choice\":\"auto\""));
    }

    #[test]
    fn zai_tool_requests_enable_tool_stream() {
        let model_provider = make_model_provider("zai", "https://api.z.ai/api/paas/v4", None);
        let req = ApiChatRequest {
            model: "glm-5".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: MessageContent::Text("List /tmp".to_string()),
            }],
            temperature: Some(0.7),
            stream: Some(false),
            stream_options: None,
            reasoning_effort: None,
            tool_stream: model_provider.tool_stream_for_tools(true),
            tools: Some(vec![serde_json::json!({
                "type": "function",
                "function": {
                    "name": "shell",
                    "description": "Run a shell command",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "command": {"type": "string"}
                        }
                    }
                }
            })]),
            tool_choice: Some("auto".to_string()),
            max_tokens: None,
        };

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"tool_stream\":true"));
    }

    #[test]
    fn non_zai_tool_requests_omit_tool_stream() {
        let model_provider = make_model_provider("test", "https://api.example.com/v1", None);
        let req = ApiChatRequest {
            model: "test-model".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: MessageContent::Text("List /tmp".to_string()),
            }],
            temperature: Some(0.7),
            stream: Some(false),
            stream_options: None,
            reasoning_effort: None,
            tool_stream: model_provider.tool_stream_for_tools(true),
            tools: Some(vec![serde_json::json!({
                "type": "function",
                "function": {
                    "name": "shell",
                    "description": "Run a shell command",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "command": {"type": "string"}
                        }
                    }
                }
            })]),
            tool_choice: Some("auto".to_string()),
            max_tokens: None,
        };

        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("\"tool_stream\""));
    }

    #[test]
    fn non_zai_provider_omits_tool_stream_regardless_of_streaming() {
        let model_provider = make_model_provider("custom", "https://proxy.example.com/v1", None);
        // tool_stream_for_tools should return None for non-Z.AI model_providers
        assert_eq!(model_provider.tool_stream_for_tools(true), None);
        assert_eq!(model_provider.tool_stream_for_tools(false), None);
    }

    #[test]
    fn z_ai_host_enables_tool_stream_for_custom_profiles() {
        let model_provider =
            make_model_provider("custom", "https://api.z.ai/api/coding/paas/v4", None);
        assert_eq!(model_provider.tool_stream_for_tools(true), Some(true));
    }

    #[test]
    fn response_with_tool_calls_deserializes() {
        let json = r#"{
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"location\":\"London\"}"
                        }
                    }]
                }
            }]
        }"#;

        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert!(msg.content.is_none());
        let tool_calls = msg.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(
            tool_calls[0].function.as_ref().unwrap().name.as_deref(),
            Some("get_weather")
        );
        assert_eq!(
            tool_calls[0]
                .function
                .as_ref()
                .unwrap()
                .arguments
                .as_deref(),
            Some("{\"location\":\"London\"}")
        );
    }

    #[test]
    fn response_with_multiple_tool_calls() {
        let json = r#"{
            "choices": [{
                "message": {
                    "content": "I'll check both.",
                    "tool_calls": [
                        {
                            "type": "function",
                            "function": {
                                "name": "get_weather",
                                "arguments": "{\"location\":\"London\"}"
                            }
                        },
                        {
                            "type": "function",
                            "function": {
                                "name": "get_time",
                                "arguments": "{\"timezone\":\"UTC\"}"
                            }
                        }
                    ]
                }
            }]
        }"#;

        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.content.as_deref(), Some("I'll check both."));
        let tool_calls = msg.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(
            tool_calls[0].function.as_ref().unwrap().name.as_deref(),
            Some("get_weather")
        );
        assert_eq!(
            tool_calls[1].function.as_ref().unwrap().name.as_deref(),
            Some("get_time")
        );
    }

    #[tokio::test]
    async fn chat_with_tools_without_key_attempts_request() {
        let p = make_model_provider("TestProvider", "http://127.0.0.1:1", None);
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
        }];
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "test_tool",
                "description": "A test tool",
                "parameters": {}
            }
        })];

        let result = p
            .chat_with_tools(&messages, &tools, "model", Some(0.7))
            .await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            !err_msg.contains("API key not set"),
            "should not get credential error, got: {err_msg}"
        );
    }

    #[test]
    fn chat_with_tools_request_preserves_reasoning_content_in_history() {
        let p = make_model_provider("DeepSeek", "https://api.deepseek.example/v1", None);
        let history_json = serde_json::json!({
            "content": "I will inspect the workspace.",
            "tool_calls": [{
                "id": "call_1",
                "name": "shell",
                "arguments": "{\"cmd\":\"ls\"}"
            }],
            "reasoning_content": "Need to inspect the current files before answering."
        });
        let messages = vec![
            ChatMessage::assistant(history_json.to_string()),
            ChatMessage::tool(r#"{"tool_call_id":"call_1","content":"src\nCargo.toml"}"#),
            ChatMessage::user("continue"),
        ];
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "shell",
                "description": "Run a shell command",
                "parameters": {}
            }
        })];

        let request = p.build_native_tool_chat_request(
            &messages,
            Some(tools),
            "deepseek-v4-flash",
            Some(0.7),
            true,
        );
        let value = serde_json::to_value(&request).unwrap();
        let first_message = &value["messages"][0];

        assert_eq!(first_message["role"], "assistant");
        assert_eq!(
            first_message["reasoning_content"],
            "Need to inspect the current files before answering."
        );
        assert!(
            first_message["tool_calls"].is_array(),
            "assistant tool-call history must stay native in chat_with_tools requests"
        );
        assert_eq!(value["tools"][0]["function"]["name"], "shell");
        assert_eq!(value["tool_choice"], "auto");
    }

    #[test]
    fn response_with_no_tool_calls_has_empty_vec() {
        let json = r#"{"choices":[{"message":{"content":"Just text, no tools."}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.content.as_deref(), Some("Just text, no tools."));
        assert!(msg.tool_calls.is_none());
    }

    #[test]
    fn flatten_system_messages_merges_into_first_user_and_removes_system_roles() {
        let messages = vec![
            ChatMessage::system("System A"),
            ChatMessage::assistant("Earlier assistant turn"),
            ChatMessage::system("System B"),
            ChatMessage::user("User turn"),
            ChatMessage::tool(r#"{"ok":true}"#),
        ];

        let flattened = OpenAiCompatibleModelProvider::flatten_system_messages(&messages, true);
        assert_eq!(flattened.len(), 3);
        assert_eq!(flattened[0].role, "assistant");
        assert_eq!(
            flattened[1].content,
            "System A\n\nSystem B\n\nUser turn".to_string()
        );
        assert_eq!(flattened[1].role, "user");
        assert_eq!(flattened[2].role, "tool");
        assert!(!flattened.iter().any(|m| m.role == "system"));
    }

    #[test]
    fn flatten_system_messages_keeps_system_only_at_start_without_user_merge() {
        let messages = vec![
            ChatMessage::system("System A"),
            ChatMessage::user("User turn"),
            ChatMessage::assistant("Assistant turn"),
            ChatMessage::system("System B"),
            ChatMessage::user("Follow-up"),
        ];

        let flattened = OpenAiCompatibleModelProvider::flatten_system_messages(&messages, false);
        assert_eq!(
            flattened
                .iter()
                .map(|message| message.role.as_str())
                .collect::<Vec<_>>(),
            vec!["system", "user", "assistant", "user"]
        );
        assert_eq!(
            flattened
                .iter()
                .filter(|message| message.role == "system")
                .count(),
            1
        );
        assert!(flattened[0].content.contains("System A"));
        assert!(flattened[0].content.contains("System B"));
    }

    #[test]
    fn flatten_system_messages_drops_empty_system_messages() {
        let messages = vec![
            ChatMessage::system(""),
            ChatMessage::user("User turn"),
            ChatMessage::system(""),
        ];

        let flattened = OpenAiCompatibleModelProvider::flatten_system_messages(&messages, false);

        assert_eq!(flattened.len(), 1);
        assert_eq!(flattened[0].role, "user");
        assert_eq!(flattened[0].content, "User turn");
    }

    #[test]
    fn flatten_system_messages_inserts_synthetic_user_when_no_user_exists() {
        let messages = vec![
            ChatMessage::assistant("Assistant only"),
            ChatMessage::system("Synthetic system"),
        ];

        let flattened = OpenAiCompatibleModelProvider::flatten_system_messages(&messages, true);
        assert_eq!(flattened.len(), 2);
        assert_eq!(flattened[0].role, "user");
        assert_eq!(flattened[0].content, "Synthetic system");
        assert_eq!(flattened[1].role, "assistant");
    }

    #[test]
    fn strip_think_tags_removes_multiple_blocks_with_surrounding_text() {
        let input = "Answer A <think>hidden 1</think> and B <think>hidden 2</think> done";
        let output = strip_think_tags(input);
        assert_eq!(output, "Answer A  and B  done");
    }

    #[test]
    fn strip_think_tags_drops_tail_for_unclosed_block() {
        let input = "Visible<think>hidden tail";
        let output = strip_think_tags(input);
        assert_eq!(output, "Visible");
    }

    // ----------------------------------------------------------
    // Reasoning model fallback tests (reasoning_content)
    // ----------------------------------------------------------

    #[test]
    fn reasoning_content_fallback_when_content_empty() {
        // Reasoning models (Qwen3, GLM-4) return content: "" with reasoning_content populated
        let json = r#"{"choices":[{"message":{"content":"","reasoning_content":"Thinking output here"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.effective_content(), "Thinking output here");
    }

    #[test]
    fn reasoning_content_fallback_when_content_null() {
        // Some models may return content: null with reasoning_content
        let json =
            r#"{"choices":[{"message":{"content":null,"reasoning_content":"Fallback text"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.effective_content(), "Fallback text");
    }

    #[test]
    fn reasoning_content_fallback_when_content_missing() {
        // content field absent entirely, reasoning_content present
        let json = r#"{"choices":[{"message":{"reasoning_content":"Only reasoning"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.effective_content(), "Only reasoning");
    }

    #[test]
    fn reasoning_content_not_used_when_content_present() {
        // Normal model: content populated, reasoning_content should be ignored
        let json = r#"{"choices":[{"message":{"content":"Normal response","reasoning_content":"Should be ignored"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.effective_content(), "Normal response");
    }

    #[test]
    fn reasoning_content_used_when_content_only_think_tags() {
        let json = r#"{"choices":[{"message":{"content":"<think>secret</think>","reasoning_content":"Fallback text"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.effective_content(), "Fallback text");
        assert_eq!(
            msg.effective_content_optional().as_deref(),
            Some("Fallback text")
        );
    }

    #[test]
    fn reasoning_content_both_absent_returns_empty() {
        // Neither content nor reasoning_content - returns empty string
        let json = r#"{"choices":[{"message":{}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.effective_content(), "");
    }

    #[test]
    fn reasoning_content_ignored_by_normal_models() {
        // Standard response without reasoning_content still works
        let json = r#"{"choices":[{"message":{"content":"Hello from Venice!"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert!(msg.reasoning_content.is_none());
        assert_eq!(msg.effective_content(), "Hello from Venice!");
    }

    // ----------------------------------------------------------
    // SSE streaming reasoning_content fallback tests
    // ----------------------------------------------------------

    #[test]
    fn parse_sse_line_with_content() {
        let line = r#"data: {"choices":[{"delta":{"content":"hello"}}]}"#;
        let result = parse_sse_line(line).unwrap().unwrap();
        assert_eq!(result.delta, "hello");
        assert!(result.reasoning.is_none());
    }

    #[test]
    fn parse_sse_line_with_reasoning_content() {
        let line = r#"data: {"choices":[{"delta":{"reasoning_content":"thinking..."}}]}"#;
        let result = parse_sse_line(line).unwrap().unwrap();
        assert!(result.delta.is_empty());
        assert_eq!(result.reasoning.as_deref(), Some("thinking..."));
    }

    #[test]
    fn parse_sse_line_with_both_prefers_content() {
        let line = r#"data: {"choices":[{"delta":{"content":"real answer","reasoning_content":"thinking..."}}]}"#;
        let result = parse_sse_line(line).unwrap().unwrap();
        assert_eq!(result.delta, "real answer");
        assert!(result.reasoning.is_none());
    }

    #[test]
    fn parse_sse_line_with_empty_content_falls_back_to_reasoning() {
        let line =
            r#"data: {"choices":[{"delta":{"content":"","reasoning_content":"thinking..."}}]}"#;
        let result = parse_sse_line(line).unwrap().unwrap();
        assert!(result.delta.is_empty());
        assert_eq!(result.reasoning.as_deref(), Some("thinking..."));
    }

    // Regression for #6584. OpenRouter and vLLM (>= v0.16.0) emit reasoning
    // under `reasoning` rather than `reasoning_content`. Both fields must
    // be accepted on deserialization.
    #[test]
    fn parse_sse_line_accepts_reasoning_alias() {
        let line = r#"data: {"choices":[{"delta":{"reasoning":"thinking via vllm..."}}]}"#;
        let result = parse_sse_line(line).unwrap().unwrap();
        assert!(result.delta.is_empty());
        assert_eq!(result.reasoning.as_deref(), Some("thinking via vllm..."));
    }

    #[test]
    fn parse_sse_line_with_empty_content_and_reasoning_alias() {
        let line = r#"data: {"choices":[{"delta":{"content":"","reasoning":"vllm thought"}}]}"#;
        let result = parse_sse_line(line).unwrap().unwrap();
        assert!(result.delta.is_empty());
        assert_eq!(result.reasoning.as_deref(), Some("vllm thought"));
    }

    #[test]
    fn response_message_accepts_reasoning_alias_on_non_stream_path() {
        // Non-stream OpenAI Chat Completions response, vLLM/OpenRouter shape.
        let json = r#"{"content":null,"reasoning":"chain-of-thought via vllm","tool_calls":null}"#;
        let msg: ResponseMessage = serde_json::from_str(json).unwrap();
        assert!(msg.content.is_none());
        assert_eq!(
            msg.reasoning_content.as_deref(),
            Some("chain-of-thought via vllm"),
            "the `reasoning` alias must populate the canonical reasoning_content field",
        );
        // effective_content should also surface the reasoning when content is missing.
        assert_eq!(msg.effective_content(), "chain-of-thought via vllm");
    }

    #[test]
    fn response_message_canonical_reasoning_content_still_works() {
        // Existing providers continue to populate reasoning_content directly.
        let json = r#"{"content":null,"reasoning_content":"canonical thought","tool_calls":null}"#;
        let msg: ResponseMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.reasoning_content.as_deref(), Some("canonical thought"));
    }

    // Review feedback on PR #6615 (Audacity88): when a payload carries BOTH
    // `reasoning_content` and `reasoning`, the previous `#[serde(alias)]`
    // version raised `duplicate field reasoning_content` at the deserializer.
    // The replacement `#[serde(from = "RawResponseMessage")]` shape must
    // accept the payload AND apply the documented precedence rule: canonical
    // `reasoning_content` wins, `reasoning` is dropped.
    #[test]
    fn response_message_with_both_keys_prefers_canonical_reasoning_content() {
        let json = r#"{"content":null,"reasoning_content":"canonical","reasoning":"alias","tool_calls":null}"#;
        let msg: ResponseMessage = serde_json::from_str(json)
            .expect("payload with both reasoning_content and reasoning must deserialize");
        assert_eq!(
            msg.reasoning_content.as_deref(),
            Some("canonical"),
            "canonical reasoning_content must win when both fields are present",
        );
    }

    #[test]
    fn response_message_with_only_alias_populates_canonical_field() {
        // Sanity: when only the alias is present, it still flows into the
        // canonical reasoning_content field.
        let json = r#"{"content":null,"reasoning":"alias only","tool_calls":null}"#;
        let msg: ResponseMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.reasoning_content.as_deref(), Some("alias only"));
    }

    #[test]
    fn stream_delta_with_both_keys_prefers_canonical_reasoning_content() {
        // The streaming-SSE shape used the same `#[serde(alias)]` and had the
        // same duplicate-field error mode. Pin the precedence here too.
        let chunk = r#"data: {"choices":[{"delta":{"reasoning_content":"canonical","reasoning":"alias"}}]}"#;
        let result = parse_sse_line(chunk)
            .expect("parse must succeed")
            .expect("non-empty chunk");
        assert_eq!(result.reasoning.as_deref(), Some("canonical"));
    }

    // The round-trip path at to_native_messages reconstructs reasoning_content
    // from session-stored assistant-with-tool-calls JSON. Both names must work.
    #[test]
    fn round_trip_reasoning_extraction_accepts_alias() {
        fn extract_reasoning(value: &serde_json::Value) -> Option<String> {
            value
                .get("reasoning_content")
                .or_else(|| value.get("reasoning"))
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
        }
        let canonical: serde_json::Value =
            serde_json::from_str(r#"{"reasoning_content":"canonical","tool_calls":[]}"#).unwrap();
        let alias: serde_json::Value =
            serde_json::from_str(r#"{"reasoning":"vllm","tool_calls":[]}"#).unwrap();
        let neither: serde_json::Value = serde_json::from_str(r#"{"tool_calls":[]}"#).unwrap();
        let both: serde_json::Value = serde_json::from_str(
            r#"{"reasoning_content":"canonical","reasoning":"alias","tool_calls":[]}"#,
        )
        .unwrap();
        assert_eq!(extract_reasoning(&canonical).as_deref(), Some("canonical"));
        assert_eq!(extract_reasoning(&alias).as_deref(), Some("vllm"));
        assert_eq!(extract_reasoning(&neither), None);
        // When both are present, the canonical name wins — preserves existing
        // behavior for providers that emit `reasoning_content` plus a stray
        // `reasoning` field.
        assert_eq!(extract_reasoning(&both).as_deref(), Some("canonical"));
    }

    #[test]
    fn parse_sse_line_done_sentinel() {
        let line = "data: [DONE]";
        let result = parse_sse_line(line).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn parse_sse_chunk_with_tool_call_delta() {
        let line = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"shell","arguments":"{\"command\":\"date\"}"}}]}}]}"#;
        let chunk = parse_sse_chunk(line)
            .unwrap()
            .expect("chunk should be parsed");
        let choice = chunk.choices.first().expect("choice should exist");
        let tool_calls = choice
            .delta
            .tool_calls
            .as_ref()
            .expect("tool call deltas should exist");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].index, Some(0));
        assert_eq!(tool_calls[0].id.as_deref(), Some("call_1"));
        assert_eq!(
            tool_calls[0]
                .function
                .as_ref()
                .and_then(|function| function.name.as_deref()),
            Some("shell")
        );
    }

    #[test]
    fn stream_tool_call_accumulator_combines_deltas() {
        let mut acc = StreamToolCallAccumulator::default();
        acc.apply_delta(&StreamToolCallDelta {
            index: Some(0),
            id: Some("call_1".to_string()),
            function: Some(StreamFunctionDelta {
                name: Some("shell".to_string()),
                arguments: Some("{\"command\":\"".to_string()),
            }),
            name: None,
            arguments: None,
            extra_content: None,
        });
        acc.apply_delta(&StreamToolCallDelta {
            index: Some(0),
            id: None,
            function: Some(StreamFunctionDelta {
                name: None,
                arguments: Some("date\"}".to_string()),
            }),
            name: None,
            arguments: None,
            extra_content: None,
        });

        let mut used_tool_call_ids = std::collections::HashSet::new();
        let tool_call = acc
            .into_provider_tool_call(false, &mut used_tool_call_ids)
            .expect("accumulator should emit tool call");
        assert_eq!(tool_call.id, "call_1");
        assert_eq!(tool_call.name, "shell");
        assert_eq!(tool_call.arguments, r#"{"command":"date"}"#);
    }

    #[test]
    fn stream_tool_call_accumulator_mistral_normalizes_invalid_id() {
        let mut acc = StreamToolCallAccumulator::default();
        acc.apply_delta(&StreamToolCallDelta {
            index: Some(0),
            id: Some("chatcmpl-tool-abc".to_string()),
            function: Some(StreamFunctionDelta {
                name: Some("shell".to_string()),
                arguments: Some(r#"{"command":"date"}"#.to_string()),
            }),
            name: None,
            arguments: None,
            extra_content: None,
        });

        let mut used_tool_call_ids = std::collections::HashSet::new();
        let tool_call = acc
            .into_provider_tool_call(true, &mut used_tool_call_ids)
            .expect("accumulator should emit tool call");

        assert_eq!(tool_call.id.len(), 9);
        assert!(tool_call.id.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(tool_call.id, "chatcmpl-tool-abc");
    }

    #[test]
    fn api_response_parses_usage() {
        let json = r#"{
            "choices": [{"message": {"content": "Hello"}}],
            "usage": {"prompt_tokens": 150, "completion_tokens": 60}
        }"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage.unwrap();
        assert_eq!(usage.prompt_tokens, Some(150));
        assert_eq!(usage.completion_tokens, Some(60));
    }

    #[test]
    fn api_response_parses_without_usage() {
        let json = r#"{"choices": [{"message": {"content": "Hello"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.usage.is_none());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // reasoning_content pass-through tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_native_response_captures_reasoning_content() {
        let provider = make_model_provider("test", "https://example.com", None);
        let message = ResponseMessage {
            content: Some("answer".to_string()),
            reasoning_content: Some("thinking step".to_string()),
            tool_calls: Some(vec![ToolCall {
                id: Some("call_1".to_string()),
                kind: Some("function".to_string()),
                function: Some(Function {
                    name: Some("shell".to_string()),
                    arguments: Some(r#"{"cmd":"ls"}"#.to_string()),
                }),
                name: None,
                arguments: None,
                parameters: None,
                extra_content: None,
            }]),
        };

        let parsed = provider.parse_native_response(message);
        assert_eq!(parsed.reasoning_content.as_deref(), Some("thinking step"));
        assert_eq!(parsed.text.as_deref(), Some("answer"));
        assert_eq!(parsed.tool_calls.len(), 1);
    }

    #[test]
    fn parse_native_response_none_reasoning_content_for_normal_model() {
        let provider = make_model_provider("test", "https://example.com", None);
        let message = ResponseMessage {
            content: Some("hello".to_string()),
            reasoning_content: None,
            tool_calls: None,
        };

        let parsed = provider.parse_native_response(message);
        assert!(parsed.reasoning_content.is_none());
        assert_eq!(parsed.text.as_deref(), Some("hello"));
    }

    #[test]
    fn convert_messages_for_native_round_trips_reasoning_content() {
        // Simulate stored assistant history JSON that includes reasoning_content
        let history_json = serde_json::json!({
            "content": "I will check",
            "tool_calls": [{
                "id": "tc_1",
                "name": "shell",
                "arguments": "{\"cmd\":\"ls\"}"
            }],
            "reasoning_content": "Let me think about this..."
        });

        let messages = vec![ChatMessage::assistant(history_json.to_string())];
        let provider = make_model_provider("test", "https://example.com", None);
        let native = provider.convert_messages_for_native(&messages, true);
        assert_eq!(native.len(), 1);
        assert_eq!(native[0].role, "assistant");
        assert_eq!(
            native[0].reasoning_content.as_deref(),
            Some("Let me think about this...")
        );
        assert!(native[0].tool_calls.is_some());
    }

    #[test]
    fn convert_messages_for_native_no_reasoning_content_when_absent() {
        // Normal model history without reasoning_content key
        let history_json = serde_json::json!({
            "content": "I will check",
            "tool_calls": [{
                "id": "tc_1",
                "name": "shell",
                "arguments": "{\"cmd\":\"ls\"}"
            }]
        });

        let messages = vec![ChatMessage::assistant(history_json.to_string())];
        let provider = make_model_provider("test", "https://example.com", None);
        let native = provider.convert_messages_for_native(&messages, true);
        assert_eq!(native.len(), 1);
        assert!(native[0].reasoning_content.is_none());
    }

    /// Regression test for #6233 — plain-text assistant turns from thinking-mode
    /// providers (DeepSeek V4) carry `reasoning_content` in JSON-encoded
    /// `content` with no `tool_calls`. The original tool-call-only branch
    /// missed this shape and the message fell through to the plain-text
    /// fallback, dropping `reasoning_content` and breaking the next request
    /// with "reasoning_content in the thinking mode must be passed back".
    #[test]
    fn convert_messages_for_native_round_trips_reasoning_content_without_tool_calls() {
        let history_json = serde_json::json!({
            "content": "Direct answer.",
            "reasoning_content": "Let me think step by step..."
        });

        let messages = vec![ChatMessage::assistant(history_json.to_string())];
        let provider = make_model_provider("test", "https://example.com", None);
        let native = provider.convert_messages_for_native(&messages, true);
        assert_eq!(native.len(), 1);
        assert_eq!(native[0].role, "assistant");
        assert!(
            native[0].tool_calls.is_none(),
            "no tool_calls on a plain-text turn"
        );
        assert_eq!(
            native[0].reasoning_content.as_deref(),
            Some("Let me think step by step...")
        );
        match &native[0].content {
            Some(MessageContent::Text(t)) => assert_eq!(t, "Direct answer."),
            other => panic!("expected text content, got {other:?}"),
        }
    }

    /// Structured-output assistant JSON with only a `content` key is user-visible
    /// answer text, not a thinking-mode replay envelope. It must stay verbatim.
    #[test]
    fn convert_messages_for_native_content_only_json_falls_through() {
        let structured_answer = serde_json::json!({"content": "raw"});
        let raw_json = structured_answer.to_string();
        let messages = vec![ChatMessage::assistant(raw_json.clone())];
        let provider = make_model_provider("test", "https://example.com", None);
        let native = provider.convert_messages_for_native(&messages, true);
        assert_eq!(native.len(), 1);
        assert!(native[0].reasoning_content.is_none());
        assert!(native[0].tool_calls.is_none());
        match &native[0].content {
            Some(MessageContent::Text(t)) => assert_eq!(t.as_str(), raw_json.as_str()),
            other => panic!("expected text content from fallback, got {other:?}"),
        }
    }

    /// `reasoning_content` must be an actual replay string. A non-string value
    /// can appear in user-authored structured JSON and must stay verbatim.
    #[test]
    fn convert_messages_for_native_non_string_reasoning_content_falls_through() {
        let structured_answer = serde_json::json!({
            "content": "raw",
            "reasoning_content": null
        });
        let raw_json = structured_answer.to_string();
        let messages = vec![ChatMessage::assistant(raw_json.clone())];
        let provider = make_model_provider("test", "https://example.com", None);
        let native = provider.convert_messages_for_native(&messages, true);
        assert_eq!(native.len(), 1);
        assert!(native[0].reasoning_content.is_none());
        assert!(native[0].tool_calls.is_none());
        match &native[0].content {
            Some(MessageContent::Text(t)) => assert_eq!(t.as_str(), raw_json.as_str()),
            other => panic!("expected text content from fallback, got {other:?}"),
        }
    }

    /// A JSON-shaped assistant message that lacks both `content` and
    /// `reasoning_content` is not a thinking-mode replay payload and must
    /// fall through to the plain-text path so the JSON survives verbatim
    /// to the wire (rather than collapsing to an empty content).
    #[test]
    fn convert_messages_for_native_unrelated_json_falls_through() {
        let unrelated = serde_json::json!({"foo": "bar"});
        let messages = vec![ChatMessage::assistant(unrelated.to_string())];
        let provider = make_model_provider("test", "https://example.com", None);
        let native = provider.convert_messages_for_native(&messages, true);
        assert_eq!(native.len(), 1);
        assert!(native[0].reasoning_content.is_none());
        assert!(native[0].tool_calls.is_none());
        match &native[0].content {
            Some(MessageContent::Text(t)) => {
                assert!(
                    t.contains("\"foo\""),
                    "expected raw JSON in fallback content, got {t:?}"
                );
            }
            other => panic!("expected text content from fallback, got {other:?}"),
        }
    }

    #[test]
    fn convert_messages_for_native_reasoning_content_serialized_only_when_present() {
        // Verify skip_serializing_if works: reasoning_content omitted from JSON when None
        let msg_without = NativeMessage {
            role: "assistant".to_string(),
            content: Some(MessageContent::Text("hi".to_string())),
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: None,
        };
        let json = serde_json::to_string(&msg_without).unwrap();
        assert!(
            !json.contains("reasoning_content"),
            "reasoning_content should be omitted when None"
        );

        let msg_with = NativeMessage {
            role: "assistant".to_string(),
            content: Some(MessageContent::Text("hi".to_string())),
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: Some("thinking...".to_string()),
        };
        let json = serde_json::to_string(&msg_with).unwrap();
        assert!(
            json.contains("reasoning_content"),
            "reasoning_content should be present when Some"
        );
        assert!(json.contains("thinking..."));
    }

    #[test]
    fn default_timeout_is_120s() {
        let p = make_model_provider("test", "https://example.com", None);
        assert_eq!(p.timeout_secs, 120);
    }

    #[test]
    fn with_timeout_secs_overrides_default() {
        let p = make_model_provider("test", "https://example.com", None).with_timeout_secs(300);
        assert_eq!(p.timeout_secs, 300);
    }

    #[test]
    fn extra_headers_default_empty() {
        let p = make_model_provider("test", "https://example.com", None);
        assert!(p.extra_headers.is_empty());
    }

    #[test]
    fn with_extra_headers_sets_headers() {
        let mut headers = std::collections::HashMap::new();
        headers.insert("X-Title".to_string(), "zeroclaw".to_string());
        headers.insert(
            "HTTP-Referer".to_string(),
            "https://example.com".to_string(),
        );
        let p =
            make_model_provider("test", "https://example.com", None).with_extra_headers(headers);
        assert_eq!(p.extra_headers.len(), 2);
        assert_eq!(p.extra_headers.get("X-Title").unwrap(), "zeroclaw");
        assert_eq!(
            p.extra_headers.get("HTTP-Referer").unwrap(),
            "https://example.com"
        );
    }

    #[test]
    fn http_client_with_extra_headers_builds_successfully() {
        let mut headers = std::collections::HashMap::new();
        headers.insert("X-Title".to_string(), "zeroclaw".to_string());
        headers.insert("User-Agent".to_string(), "TestAgent/1.0".to_string());
        let p =
            make_model_provider("test", "https://example.com", None).with_extra_headers(headers);
        // Should not panic
        let _client = p.http_client();
    }

    #[test]
    fn http_client_without_extra_headers_or_user_agent() {
        let p = make_model_provider("test", "https://example.com", None);
        // Should use the cached proxy client path
        let _client = p.http_client();
    }

    #[test]
    fn extra_headers_combined_with_user_agent() {
        let mut headers = std::collections::HashMap::new();
        headers.insert("X-Title".to_string(), "zeroclaw".to_string());
        let p = OpenAiCompatibleModelProvider::new_with_user_agent(
            "test",
            "test",
            "https://example.com",
            None,
            AuthStyle::Bearer,
            "CustomAgent/1.0",
        )
        .with_extra_headers(headers);
        assert_eq!(p.user_agent.as_deref(), Some("CustomAgent/1.0"));
        assert_eq!(p.extra_headers.len(), 1);
        // Should not panic
        let _client = p.http_client();
    }

    #[test]
    fn tool_call_none_fields_omitted_from_json() {
        // Ensures model_providers like Mistral that reject extra fields (e.g. "name": null)
        // don't receive them when the ToolCall compat fields are None.
        let tc = ToolCall {
            id: Some("call_1".to_string()),
            kind: Some("function".to_string()),
            function: Some(Function {
                name: Some("shell".to_string()),
                arguments: Some("{\"command\":\"ls\"}".to_string()),
            }),
            name: None,
            arguments: None,
            parameters: None,
            extra_content: None,
        };
        let json = serde_json::to_value(&tc).unwrap();
        assert!(!json.as_object().unwrap().contains_key("name"));
        assert!(!json.as_object().unwrap().contains_key("arguments"));
        assert!(!json.as_object().unwrap().contains_key("parameters"));
        // Standard fields must be present
        assert!(json.as_object().unwrap().contains_key("id"));
        assert!(json.as_object().unwrap().contains_key("type"));
        assert!(json.as_object().unwrap().contains_key("function"));
    }

    #[test]
    fn tool_call_with_compat_fields_serializes_them() {
        // When compat fields are Some, they should appear in the output.
        let tc = ToolCall {
            id: None,
            kind: None,
            function: None,
            name: Some("shell".to_string()),
            arguments: Some("{\"command\":\"ls\"}".to_string()),
            parameters: None,
            extra_content: None,
        };
        let json = serde_json::to_value(&tc).unwrap();
        assert_eq!(json["name"], "shell");
        assert_eq!(json["arguments"], "{\"command\":\"ls\"}");
        // None fields should be omitted
        assert!(!json.as_object().unwrap().contains_key("id"));
        assert!(!json.as_object().unwrap().contains_key("type"));
        assert!(!json.as_object().unwrap().contains_key("function"));
        assert!(!json.as_object().unwrap().contains_key("parameters"));
    }

    // ── parse_proxy_tool_event tests ──

    #[test]
    fn proxy_tool_start_valid() {
        let line = r#"data: {"x_tool_start":{"name":"bash","arguments":"{\"cmd\":\"ls\"}"}}"#;
        let event = parse_proxy_tool_event(line);
        assert!(matches!(
            event,
            Some(StreamEvent::PreExecutedToolCall { ref name, ref args })
            if name == "bash" && args == r#"{"cmd":"ls"}"#
        ));
    }

    #[test]
    fn proxy_tool_start_missing_name_returns_none() {
        let line = r#"data: {"x_tool_start":{"arguments":"{}"}}"#;
        assert!(parse_proxy_tool_event(line).is_none());
    }

    #[test]
    fn proxy_tool_start_missing_arguments_defaults() {
        let line = r#"data: {"x_tool_start":{"name":"read"}}"#;
        let event = parse_proxy_tool_event(line);
        assert!(matches!(
            event,
            Some(StreamEvent::PreExecutedToolCall { ref name, ref args })
            if name == "read" && args == "{}"
        ));
    }

    #[test]
    fn proxy_tool_result_valid() {
        let line = r#"data: {"x_tool_result":{"name":"bash","output":"hello world"}}"#;
        let event = parse_proxy_tool_event(line);
        assert!(matches!(
            event,
            Some(StreamEvent::PreExecutedToolResult { ref name, ref output })
            if name == "bash" && output == "hello world"
        ));
    }

    #[test]
    fn proxy_tool_result_missing_fields_uses_defaults() {
        let line = r#"data: {"x_tool_result":{}}"#;
        let event = parse_proxy_tool_event(line);
        assert!(matches!(
            event,
            Some(StreamEvent::PreExecutedToolResult { ref name, ref output })
            if name == "unknown" && output.is_empty()
        ));
    }

    #[test]
    fn proxy_tool_event_non_json_returns_none() {
        assert!(parse_proxy_tool_event("data: not json").is_none());
    }

    #[test]
    fn proxy_tool_event_no_data_prefix_returns_none() {
        let line = r#"{"x_tool_start":{"name":"bash"}}"#;
        assert!(parse_proxy_tool_event(line).is_none());
    }

    #[test]
    fn proxy_tool_event_standard_openai_chunk_returns_none() {
        let line = r#"data: {"id":"chatcmpl-1","choices":[{"delta":{"content":"hi"}}]}"#;
        assert!(parse_proxy_tool_event(line).is_none());
    }

    #[test]
    fn proxy_tool_event_done_sentinel_returns_none() {
        assert!(parse_proxy_tool_event("data: [DONE]").is_none());
    }

    /// Regression for #5825.
    ///
    /// When `native_tool_calling = false`, the filter pass rewrites
    /// `assistant{tool_calls, content="I'll search"}` into `assistant("I'll
    /// search")` and drops the following `tool{result}`. That leaves two
    /// adjacent assistant messages in the output, which model_providers targeted
    /// by this path (Anthropic upstream, MiniMax, other OpenAI-compat
    /// wrappers) reject with HTTP 400.
    #[test]
    fn strip_native_tool_messages_coalesces_adjacent_assistants() {
        let messages = vec![
            ChatMessage::user("search for cats"),
            ChatMessage::assistant(
                r#"{"content":"I'll search","tool_calls":[{"id":"t1","name":"web_search","arguments":"{}"}]}"#,
            ),
            ChatMessage::tool(r#"{"tool_call_id":"t1","content":"Found 10 results"}"#),
            ChatMessage::assistant("Here are the results about cats"),
        ];
        let p = OpenAiCompatibleModelProvider::new_merge_system_into_user(
            "test",
            "MiniMax",
            "https://api.minimax.chat/v1",
            Some("k"),
            AuthStyle::Bearer,
        );
        let stripped = p.strip_native_tool_messages(&messages);
        let roles: Vec<&str> = stripped.iter().map(|m| m.role.as_str()).collect();
        assert!(
            !roles.windows(2).any(|w| w[0] == w[1]),
            "no two consecutive messages should share a role; got {roles:?}"
        );
        // Sanity: user turn and merged assistant content both survive.
        assert_eq!(roles, vec!["user", "assistant"]);
        assert_eq!(stripped[0].content, "search for cats");
        assert!(
            stripped[1].content.contains("I'll search")
                && stripped[1]
                    .content
                    .contains("Here are the results about cats"),
            "merged assistant should preserve both the pre-tool narration and the final reply; \
             got {:?}",
            stripped[1].content
        );
    }

    /// Complementary regression for #5825: when the narration content is
    /// empty, the pre-tool assistant is dropped entirely and no coalesce is
    /// needed. This test documents that the coalesce pass does not produce
    /// spurious blank-line concatenation.
    #[test]
    fn strip_native_tool_messages_drops_empty_narration_cleanly() {
        let messages = vec![
            ChatMessage::user("search for cats"),
            ChatMessage::assistant(
                r#"{"content":"","tool_calls":[{"id":"t1","name":"web_search","arguments":"{}"}]}"#,
            ),
            ChatMessage::tool(r#"{"tool_call_id":"t1","content":"Found"}"#),
            ChatMessage::assistant("Here are the results"),
        ];
        let p = OpenAiCompatibleModelProvider::new_merge_system_into_user(
            "test",
            "MiniMax",
            "https://api.minimax.chat/v1",
            Some("k"),
            AuthStyle::Bearer,
        );
        let stripped = p.strip_native_tool_messages(&messages);
        assert_eq!(
            stripped.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
            vec!["user", "assistant"]
        );
        assert_eq!(stripped[1].content, "Here are the results");
    }

    /// Integration regression: dropping the `stream_chat` result must abort the
    /// forwarding task and close the upstream socket. A bare `unfold(rx)` leaks
    /// it; the isolated guard unit test would not catch that.
    #[tokio::test]
    async fn dropping_stream_aborts_forwarder_and_closes_upstream_socket() {
        use axum::Router;
        use axum::response::IntoResponse;
        use axum::routing::post;
        use futures_util::StreamExt as _;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio::net::TcpListener;

        let handler_dropped = Arc::new(AtomicBool::new(false));
        let handler_dropped_for_route = Arc::clone(&handler_dropped);

        let app = Router::new().route(
            "/chat/completions",
            post(move || {
                let dropped = Arc::clone(&handler_dropped_for_route);
                async move {
                    let sentinel = scopeguard::guard((), move |()| {
                        dropped.store(true, Ordering::SeqCst);
                    });
                    let first = futures_util::stream::once(async {
                        Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(
                            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
                        ))
                    });
                    let park = futures_util::stream::poll_fn(move |_cx| {
                        let _ = &sentinel;
                        std::task::Poll::Pending
                    });
                    axum::body::Body::from_stream(first.chain(park)).into_response()
                }
            }),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let provider = OpenAiCompatibleModelProvider::new(
            "test",
            "test",
            &format!("http://{addr}"),
            Some("k"),
            AuthStyle::Bearer,
        );

        let mut stream = provider.stream_chat(
            crate::traits::ChatRequest {
                messages: &[ChatMessage::user("hi")],
                tools: None,
                thinking: None,
            },
            "gpt-test",
            Some(0.0),
            StreamOptions {
                enabled: true,
                count_tokens: false,
            },
        );

        let first = stream.next().await;
        assert!(first.is_some(), "expected at least the first SSE chunk");

        drop(stream);

        let observed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if handler_dropped.load(Ordering::SeqCst) {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;

        server.abort();
        assert!(
            observed.is_ok(),
            "dropped stream must abort the forwarder and close the upstream socket"
        );
    }

    #[test]
    fn compatible_model_omits_temperature_matches_kimi_k2() {
        assert!(compatible_model_omits_temperature("kimi-k2.5"));
        assert!(compatible_model_omits_temperature("kimi-k2.6"));
        assert!(compatible_model_omits_temperature("kimi-k2.7"));
        assert!(compatible_model_omits_temperature("kimi-k2"));
    }

    #[test]
    fn compatible_model_omits_temperature_skips_other_models() {
        assert!(!compatible_model_omits_temperature("kimi-k1"));
        assert!(!compatible_model_omits_temperature("kimi-latest"));
        assert!(!compatible_model_omits_temperature("gpt-4o"));
        assert!(!compatible_model_omits_temperature("claude-sonnet-4-6"));
        assert!(!compatible_model_omits_temperature("llama-3.1-70b"));
    }
}
