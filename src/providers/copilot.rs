//! GitHub Copilot provider with OAuth device-flow authentication.
//!
//! Authenticates via GitHub's device code flow (same as VS Code Copilot),
//! then exchanges the OAuth token for short-lived Copilot API keys.
//! Tokens are cached to disk and auto-refreshed.
//!
//! **Note:** This uses VS Code's OAuth client ID (`Iv1.b507a08c87ecfe98`) and
//! editor headers. This is the same approach used by LiteLLM, Codex CLI,
//! and other third-party Copilot integrations. The Copilot token endpoint is
//! private; there is no public OAuth scope or app registration for it.
//! GitHub could change or revoke this at any time, which would break all
//! third-party integrations simultaneously.

use crate::auth::{self, AuthService};
use crate::providers::ProviderRuntimeOptions;
use crate::providers::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    Provider, TokenUsage, ToolCall as ProviderToolCall,
};
use crate::tools::ToolSpec;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::warn;

const GITHUB_API_KEY_URL: &str = "https://api.github.com/copilot_internal/v2/token";
const DEFAULT_API: &str = "https://api.githubcopilot.com";

/// Canonical Copilot model choices shared by onboarding and runtime model selection.
pub const COPILOT_MODEL_CHOICES: &[(&str, &str)] = &[
    (
        "gpt-5.4-mini",
        "GPT-5.4 Mini (recommended: balanced cost/latency)",
    ),
    ("gpt-5.4", "GPT-5.4 (latest flagship)"),
    ("gpt-5.3", "GPT-5.3 (high-quality)"),
    ("gpt-5.3-codex", "GPT-5.3 Codex (coding specialist)"),
    ("gpt-5.2", "GPT-5.2"),
    ("gpt-5.2-codex", "GPT-5.2 Codex (agentic coding)"),
    ("gpt-5.1", "GPT-5.1"),
    ("gpt-5.1-codex", "GPT-5.1 Codex"),
    ("gpt-5.1-codex-max", "GPT-5.1 Codex Max"),
    ("gpt-5-mini", "GPT-5 Mini"),
    ("gpt-4.1", "GPT-4.1"),
    ("gpt-4o", "GPT-4o"),
    ("claude-opus-4.6", "Claude Opus 4.6"),
    ("claude-opus-4.5", "Claude Opus 4.5"),
    ("claude-sonnet-4.5", "Claude Sonnet 4.5"),
    ("claude-haiku-4.5", "Claude Haiku 4.5"),
    ("gemini-3.1-pro", "Gemini 3.1 Pro"),
    ("gemini-3-pro", "Gemini 3 Pro"),
    ("gemini-3-flash", "Gemini 3 Flash"),
    ("gemini-2.5-pro", "Gemini 2.5 Pro"),
    ("grok-code-fast-1", "Grok Code Fast 1"),
    ("gpt-4.1-mini", "GPT-4.1 Mini (fast)"),
    ("gpt-4.1-nano", "GPT-4.1 Nano (ultra-fast)"),
    ("o1", "o1 (reasoning)"),
    ("o1-mini", "o1-mini (smaller reasoning)"),
    ("o3-mini", "o3-mini (efficient reasoning)"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopilotTransportApi {
    AnthropicMessages,
    OpenAiResponses,
}

fn resolve_copilot_transport_api(
    original_model: &str,
    normalized_model: &str,
) -> CopilotTransportApi {
    let orig = original_model.trim();
    if let Some((provider, _)) = orig.split_once('/') {
        if provider.eq_ignore_ascii_case("anthropic") {
            return CopilotTransportApi::AnthropicMessages;
        }
    }

    let orig_lc = orig.to_ascii_lowercase();
    let norm_lc = normalized_model.trim().to_ascii_lowercase();
    if orig_lc.contains("anthropic") || norm_lc.contains("claude") {
        CopilotTransportApi::AnthropicMessages
    } else {
        CopilotTransportApi::OpenAiResponses
    }
}

fn normalize_model_id(model_id: &str) -> String {
    model_id.rsplit('/').next().unwrap().trim().to_string()
}

// ── Token types ──────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct ApiKeyInfo {
    token: String,
    expires_at: i64,
    #[serde(default)]
    endpoints: Option<ApiEndpoints>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ApiEndpoints {
    api: Option<String>,
}

struct CachedApiKey {
    token: String,
    api_endpoint: String,
    expires_at: i64,
}

// ── Chat completions types ───────────────────────────────────────

#[derive(Debug, Serialize)]
struct ApiChatRequest<'a> {
    model: String,
    messages: Vec<ApiMessage>,
    temperature: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<NativeToolSpec<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
}

#[derive(Debug, Serialize)]
struct ApiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<ApiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<NativeToolCall>>,
}

#[derive(Debug, Serialize)]
struct NativeToolSpec<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    function: NativeToolFunctionSpec<'a>,
}

#[derive(Debug, Serialize)]
struct NativeToolFunctionSpec<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct NativeToolCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    function: NativeFunctionCall,
}

#[derive(Debug, Serialize, Deserialize)]
struct NativeFunctionCall {
    name: String,
    arguments: String,
}

/// Multi-part content for vision messages (OpenAI format).
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum ApiContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
enum ContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrlDetail },
}

#[derive(Debug, Clone, Serialize)]
struct ImageUrlDetail {
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

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<NativeToolCall>>,
}

#[derive(Debug, Clone, Serialize)]
struct CacheControlOut {
    #[serde(rename = "type")]
    cache_type: &'static str,
}

impl CacheControlOut {
    fn ephemeral() -> Self {
        Self {
            cache_type: "ephemeral",
        }
    }
}

#[derive(Debug, Serialize)]
struct CopilotAnthropicSystemBlock {
    #[serde(rename = "type")]
    block_type: &'static str,
    text: String,
    cache_control: CacheControlOut,
}

#[derive(Debug, Serialize)]
struct CopilotAnthropicRequest<'a> {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<Vec<CopilotAnthropicSystemBlock>>,
    messages: Vec<CopilotAnthropicMessage>,
    temperature: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<CopilotAnthropicToolSpec<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct CopilotAnthropicMessage {
    role: String,
    content: Vec<CopilotAnthropicContentOut>,
}

#[derive(Debug, Serialize)]
struct CopilotImageSource {
    #[serde(rename = "type")]
    source_type: &'static str,
    media_type: String,
    data: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum CopilotAnthropicContentOut {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControlOut>,
    },
    #[serde(rename = "image")]
    Image { source: CopilotImageSource },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, Serialize)]
struct CopilotAnthropicToolSpec<'a> {
    name: &'a str,
    description: &'a str,
    input_schema: &'a serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct CopilotAnthropicResponse {
    #[serde(default)]
    content: Vec<CopilotAnthropicContentIn>,
    #[serde(default)]
    usage: Option<CopilotAnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct CopilotAnthropicUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct CopilotAnthropicContentIn {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<serde_json::Value>,
}

// ── Provider ─────────────────────────────────────────────────────

/// GitHub Copilot provider with automatic OAuth and token refresh.
///
/// On first use, prompts the user to visit github.com/login/device.
/// Tokens are cached to `~/.config/zeroclaw/copilot/` and refreshed
/// automatically.
pub struct CopilotProvider {
    github_token: Option<String>,
    auth: AuthService,
    auth_profile_override: Option<String>,
    /// Mutex ensures only one caller refreshes tokens at a time,
    /// preventing duplicate device flow prompts or redundant API calls.
    refresh_lock: Arc<Mutex<Option<CachedApiKey>>>,
    token_dir: PathBuf,
}

fn default_zeroclaw_dir() -> PathBuf {
    directories::UserDirs::new().map_or_else(
        || PathBuf::from(".zeroclaw"),
        |dirs| dirs.home_dir().join(".zeroclaw"),
    )
}

impl CopilotProvider {
    pub fn new(options: &ProviderRuntimeOptions, github_token: Option<&str>) -> Self {
        let state_dir = options
            .zeroclaw_dir
            .clone()
            .unwrap_or_else(default_zeroclaw_dir);
        let auth = AuthService::new(&state_dir, options.secrets_encrypt);

        let token_dir = directories::ProjectDirs::from("", "", "zeroclaw")
            .map(|dir| dir.config_dir().join("copilot"))
            .unwrap_or_else(|| {
                // Fall back to a user-specific temp directory to avoid
                // shared-directory symlink attacks.
                let user = std::env::var("USER")
                    .or_else(|_| std::env::var("USERNAME"))
                    .unwrap_or_else(|_| "unknown".to_string());
                std::env::temp_dir().join(format!("zeroclaw-copilot-{user}"))
            });

        if let Err(err) = std::fs::create_dir_all(&token_dir) {
            warn!(
                "Failed to create Copilot token directory {:?}: {err}. Token caching is disabled.",
                token_dir
            );
        } else {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                if let Err(err) =
                    std::fs::set_permissions(&token_dir, std::fs::Permissions::from_mode(0o700))
                {
                    warn!(
                        "Failed to set Copilot token directory permissions on {:?}: {err}",
                        token_dir
                    );
                }
            }
        }

        Self {
            github_token: github_token
                .filter(|token| !token.is_empty())
                .map(String::from),
            auth,
            auth_profile_override: options.auth_profile_override.clone(),
            refresh_lock: Arc::new(Mutex::new(None)),
            token_dir,
        }
    }

    fn http_client(&self) -> Client {
        crate::config::build_runtime_proxy_client_with_timeouts("provider.copilot", 120, 10)
    }

    /// Required headers for Copilot API requests (editor identification).
    const COPILOT_HEADERS: [(&str, &str); 3] = [
        ("Editor-Version", "vscode/1.96.2"),
        ("User-Agent", "GitHubCopilotChat/0.26.7"),
        ("X-Github-Api-Version", "2025-04-01"),
    ];

    fn apply_copilot_headers(mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        for (header, value) in &Self::COPILOT_HEADERS {
            req = req.header(*header, *value);
        }
        req
    }

    fn profile_name_for_store(&self) -> String {
        self.auth_profile_override
            .as_deref()
            .and_then(|override_id| override_id.split_once(':').map(|(_, profile)| profile))
            .or(self.auth_profile_override.as_deref())
            .map(str::trim)
            .filter(|profile| !profile.is_empty())
            .unwrap_or("github")
            .to_string()
    }

    fn convert_tools(tools: Option<&[ToolSpec]>) -> Option<Vec<NativeToolSpec<'_>>> {
        tools.map(|items| {
            items
                .iter()
                .map(|tool| NativeToolSpec {
                    kind: "function",
                    function: NativeToolFunctionSpec {
                        name: &tool.name,
                        description: &tool.description,
                        parameters: &tool.parameters,
                    },
                })
                .collect()
        })
    }

    /// Convert message content to API format, with multi-part support for
    /// user messages containing `[IMAGE:...]` markers.
    fn to_api_content(role: &str, content: &str) -> Option<ApiContent> {
        if role != "user" {
            return Some(ApiContent::Text(content.to_string()));
        }

        let (cleaned_text, image_refs) = crate::multimodal::parse_image_markers(content);
        if image_refs.is_empty() {
            return Some(ApiContent::Text(content.to_string()));
        }

        let mut parts = Vec::with_capacity(image_refs.len() + 1);
        let trimmed = cleaned_text.trim();
        if !trimmed.is_empty() {
            parts.push(ContentPart::Text {
                text: trimmed.to_string(),
            });
        }
        for image_ref in image_refs {
            parts.push(ContentPart::ImageUrl {
                image_url: ImageUrlDetail { url: image_ref },
            });
        }

        Some(ApiContent::Parts(parts))
    }

    fn convert_messages(messages: &[ChatMessage]) -> Vec<ApiMessage> {
        messages
            .iter()
            .map(|message| {
                if message.role == "assistant" {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content) {
                        if let Some(tool_calls_value) = value.get("tool_calls") {
                            if let Ok(parsed_calls) =
                                serde_json::from_value::<Vec<ProviderToolCall>>(tool_calls_value.clone())
                            {
                                let tool_calls = parsed_calls
                                    .into_iter()
                                    .map(|tool_call| NativeToolCall {
                                        id: Some(tool_call.id),
                                        kind: Some("function".to_string()),
                                        function: NativeFunctionCall {
                                            name: tool_call.name,
                                            arguments: tool_call.arguments,
                                        },
                                    })
                                    .collect::<Vec<_>>();

                                let content = value
                                    .get("content")
                                    .and_then(serde_json::Value::as_str)
                                    .map(|s| ApiContent::Text(s.to_string()));

                                return ApiMessage {
                                    role: "assistant".to_string(),
                                    content,
                                    tool_call_id: None,
                                    tool_calls: Some(tool_calls),
                                };
                            }
                        }
                    }
                }

                if message.role == "tool" {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content) {
                        let tool_call_id = value
                            .get("tool_call_id")
                            .and_then(serde_json::Value::as_str)
                            .map(ToString::to_string);
                        let content = value
                            .get("content")
                            .and_then(serde_json::Value::as_str)
                            .map(|s| ApiContent::Text(s.to_string()));

                        return ApiMessage {
                            role: "tool".to_string(),
                            content,
                            tool_call_id,
                            tool_calls: None,
                        };
                    }
                }

                ApiMessage {
                    role: message.role.clone(),
                    content: Self::to_api_content(&message.role, &message.content),
                    tool_call_id: None,
                    tool_calls: None,
                }
            })
            .collect()
    }

    fn convert_anthropic_tools(
        tools: Option<&[ToolSpec]>,
    ) -> Option<Vec<CopilotAnthropicToolSpec<'_>>> {
        let items = tools?;
        if items.is_empty() {
            return None;
        }

        Some(
            items
                .iter()
                .map(|tool| CopilotAnthropicToolSpec {
                    name: &tool.name,
                    description: &tool.description,
                    input_schema: &tool.parameters,
                })
                .collect(),
        )
    }

    fn parse_assistant_anthropic_message(content: &str) -> Option<Vec<CopilotAnthropicContentOut>> {
        let value = serde_json::from_str::<serde_json::Value>(content).ok()?;
        let tool_calls = value
            .get("tool_calls")
            .and_then(|v| serde_json::from_value::<Vec<ProviderToolCall>>(v.clone()).ok())?;

        let mut blocks = Vec::new();
        if let Some(text) = value
            .get("content")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            blocks.push(CopilotAnthropicContentOut::Text {
                text: text.to_string(),
                cache_control: None,
            });
        }

        for call in tool_calls {
            let input = serde_json::from_str::<serde_json::Value>(&call.arguments)
                .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
            blocks.push(CopilotAnthropicContentOut::ToolUse {
                id: call.id,
                name: call.name,
                input,
            });
        }

        Some(blocks)
    }

    fn parse_anthropic_tool_result_message(content: &str) -> Option<CopilotAnthropicMessage> {
        let value = serde_json::from_str::<serde_json::Value>(content).ok()?;
        let tool_use_id = value
            .get("tool_call_id")
            .and_then(serde_json::Value::as_str)?
            .to_string();
        let output = value
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();

        Some(CopilotAnthropicMessage {
            role: "user".to_string(),
            content: vec![CopilotAnthropicContentOut::ToolResult {
                tool_use_id,
                content: output,
            }],
        })
    }

    async fn anthropic_image_block(image_ref: &str) -> Option<CopilotAnthropicContentOut> {
        if image_ref.starts_with("data:") {
            let comma = image_ref.find(',')?;
            let header = &image_ref[5..comma];
            let mime = header.split(';').next().unwrap_or("image/jpeg").to_string();
            let data = image_ref[comma + 1..].trim().to_string();
            if data.is_empty() {
                return None;
            }
            return Some(CopilotAnthropicContentOut::Image {
                source: CopilotImageSource {
                    source_type: "base64",
                    media_type: mime,
                    data,
                },
            });
        }

        let path = std::path::Path::new(image_ref.trim());
        if !path.exists() {
            return None;
        }

        let bytes = tokio::fs::read(path).await.ok()?;
        let data = {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(bytes)
        };

        let media_type = match path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("jpg")
            .to_ascii_lowercase()
            .as_str()
        {
            "png" => "image/png",
            "gif" => "image/gif",
            "webp" => "image/webp",
            _ => "image/jpeg",
        }
        .to_string();

        Some(CopilotAnthropicContentOut::Image {
            source: CopilotImageSource {
                source_type: "base64",
                media_type,
                data,
            },
        })
    }

    async fn anthropic_user_content(content: &str) -> Vec<CopilotAnthropicContentOut> {
        let (text, image_refs) = crate::multimodal::parse_image_markers(content);
        let mut blocks = Vec::new();

        for image_ref in image_refs {
            if let Some(image_block) = Self::anthropic_image_block(&image_ref).await {
                blocks.push(image_block);
            }
        }

        if blocks.is_empty() || !text.trim().is_empty() {
            let text_value = if text.trim().is_empty() {
                content.to_string()
            } else {
                text
            };
            blocks.push(CopilotAnthropicContentOut::Text {
                text: text_value,
                cache_control: None,
            });
        }

        blocks
    }

    fn push_or_merge_anthropic_message(
        messages: &mut Vec<CopilotAnthropicMessage>,
        message: CopilotAnthropicMessage,
    ) {
        if messages
            .last()
            .is_some_and(|existing| existing.role == message.role)
        {
            if let Some(last) = messages.last_mut() {
                last.content.extend(message.content);
            }
        } else {
            messages.push(message);
        }
    }

    async fn convert_messages_for_anthropic(
        messages: &[ChatMessage],
    ) -> (
        Option<Vec<CopilotAnthropicSystemBlock>>,
        Vec<CopilotAnthropicMessage>,
    ) {
        let mut system_text: Option<String> = None;
        let mut native_messages = Vec::new();

        for message in messages {
            match message.role.as_str() {
                "system" => {
                    if system_text.is_none() {
                        system_text = Some(message.content.clone());
                    }
                }
                "assistant" => {
                    let assistant_message = if let Some(blocks) =
                        Self::parse_assistant_anthropic_message(&message.content)
                    {
                        if blocks.is_empty() {
                            None
                        } else {
                            Some(CopilotAnthropicMessage {
                                role: "assistant".to_string(),
                                content: blocks,
                            })
                        }
                    } else if !message.content.trim().is_empty() {
                        Some(CopilotAnthropicMessage {
                            role: "assistant".to_string(),
                            content: vec![CopilotAnthropicContentOut::Text {
                                text: message.content.clone(),
                                cache_control: None,
                            }],
                        })
                    } else {
                        None
                    };

                    if let Some(assistant_message) = assistant_message {
                        Self::push_or_merge_anthropic_message(
                            &mut native_messages,
                            assistant_message,
                        );
                    }
                }
                "tool" => {
                    if let Some(tool_message) =
                        Self::parse_anthropic_tool_result_message(&message.content)
                    {
                        Self::push_or_merge_anthropic_message(&mut native_messages, tool_message);
                    }
                }
                _ => {
                    let user_blocks = Self::anthropic_user_content(&message.content).await;
                    let user_message = CopilotAnthropicMessage {
                        role: "user".to_string(),
                        content: user_blocks,
                    };
                    Self::push_or_merge_anthropic_message(&mut native_messages, user_message);
                }
            }
        }

        let system = system_text.map(|text| {
            vec![CopilotAnthropicSystemBlock {
                block_type: "text",
                text,
                cache_control: CacheControlOut::ephemeral(),
            }]
        });

        (system, native_messages)
    }

    async fn send_openai_chat_request(
        &self,
        messages: Vec<ApiMessage>,
        tools: Option<&[ToolSpec]>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ProviderChatResponse> {
        let (token, endpoint) = self.get_api_key().await?;
        let url = format!("{}/chat/completions", endpoint.trim_end_matches('/'));

        let native_tools = Self::convert_tools(tools);
        let request = ApiChatRequest {
            model: model.to_string(),
            messages,
            temperature,
            tool_choice: native_tools.as_ref().map(|_| "auto".to_string()),
            tools: native_tools,
        };

        let req = self
            .http_client()
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/json")
            .json(&request);

        let req = Self::apply_copilot_headers(req);

        let response = req.send().await?;

        if !response.status().is_success() {
            return Err(super::api_error("GitHub Copilot", response).await);
        }

        let api_response: ApiChatResponse = response.json().await?;
        let usage = api_response.usage.map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cached_input_tokens: None,
        });
        let choice = api_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No response from GitHub Copilot"))?;

        let tool_calls = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tool_call| ProviderToolCall {
                id: tool_call
                    .id
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                name: tool_call.function.name,
                arguments: tool_call.function.arguments,
            })
            .collect();

        Ok(ProviderChatResponse {
            text: choice.message.content,
            tool_calls,
            usage,
            reasoning_content: None,
        })
    }

    async fn send_anthropic_chat_request(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolSpec]>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ProviderChatResponse> {
        let (token, endpoint) = self.get_api_key().await?;
        let url = format!("{}/v1/messages", endpoint.trim_end_matches('/'));

        let (system, native_messages) = Self::convert_messages_for_anthropic(messages).await;
        let native_tools = Self::convert_anthropic_tools(tools);

        let request = CopilotAnthropicRequest {
            model: model.to_string(),
            max_tokens: Self::anthropic_max_tokens(),
            system,
            messages: native_messages,
            temperature,
            tools: native_tools,
            tool_choice: tools
                .filter(|items| !items.is_empty())
                .map(|_| serde_json::json!({ "type": "auto" })),
        };

        let req = self
            .http_client()
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&request);

        let req = Self::apply_copilot_headers(req);

        let response = req.send().await?;

        if !response.status().is_success() {
            return Err(super::api_error("GitHub Copilot", response).await);
        }

        let api_response: CopilotAnthropicResponse = response.json().await?;

        let usage = api_response.usage.map(|u| TokenUsage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cached_input_tokens: u.cache_read_input_tokens,
        });

        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for block in api_response.content {
            match block.kind.as_str() {
                "text" => {
                    if let Some(text) = block.text.map(|value| value.trim().to_string()) {
                        if !text.is_empty() {
                            text_parts.push(text);
                        }
                    }
                }
                "tool_use" => {
                    let name = block.name.unwrap_or_default();
                    if name.is_empty() {
                        continue;
                    }
                    tool_calls.push(ProviderToolCall {
                        id: block.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                        name,
                        arguments: block
                            .input
                            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()))
                            .to_string(),
                    });
                }
                _ => {}
            }
        }

        Ok(ProviderChatResponse {
            text: if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.join("\n"))
            },
            tool_calls,
            usage,
            reasoning_content: None,
        })
    }

    /// Send a Copilot request, selecting API transport from the model id.
    async fn send_chat_request(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolSpec]>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ProviderChatResponse> {
        let normalized_model = normalize_model_id(model);

        match resolve_copilot_transport_api(model, &normalized_model) {
            CopilotTransportApi::OpenAiResponses => {
                self.send_openai_chat_request(
                    Self::convert_messages(messages),
                    tools,
                    &normalized_model,
                    temperature,
                )
                .await
            }
            CopilotTransportApi::AnthropicMessages => {
                self.send_anthropic_chat_request(messages, tools, &normalized_model, temperature)
                    .await
            }
        }
    }

    /// Get a valid Copilot API key, refreshing or re-authenticating as needed.
    /// Uses a Mutex to ensure only one caller refreshes at a time.
    async fn get_api_key(&self) -> anyhow::Result<(String, String)> {
        let mut cached = self.refresh_lock.lock().await;

        if let Some(cached_key) = cached.as_ref() {
            if chrono::Utc::now().timestamp() + 120 < cached_key.expires_at {
                return Ok((cached_key.token.clone(), cached_key.api_endpoint.clone()));
            }
        }

        if let Some(info) = self.load_api_key_from_disk().await {
            if chrono::Utc::now().timestamp() + 120 < info.expires_at {
                let endpoint =
                    Self::resolve_copilot_api_endpoint(&info.token, info.endpoints.as_ref());
                let token = info.token;

                *cached = Some(CachedApiKey {
                    token: token.clone(),
                    api_endpoint: endpoint.clone(),
                    expires_at: info.expires_at,
                });
                return Ok((token, endpoint));
            }
        }

        let access_token = self.get_github_access_token().await?;
        let api_key_info = self.exchange_for_api_key(&access_token).await?;
        self.save_api_key_to_disk(&api_key_info).await;

        let endpoint = Self::resolve_copilot_api_endpoint(
            &api_key_info.token,
            api_key_info.endpoints.as_ref(),
        );

        *cached = Some(CachedApiKey {
            token: api_key_info.token.clone(),
            api_endpoint: endpoint.clone(),
            expires_at: api_key_info.expires_at,
        });

        Ok((api_key_info.token, endpoint))
    }

    fn derive_copilot_api_base_url_from_token(token: &str) -> Option<String> {
        let proxy_endpoint = token
            .split(';')
            .find_map(|part| part.trim().strip_prefix("proxy-ep="))
            .map(str::trim)
            .filter(|value| !value.is_empty())?;

        let parsed =
            if proxy_endpoint.starts_with("http://") || proxy_endpoint.starts_with("https://") {
                reqwest::Url::parse(proxy_endpoint).ok()?
            } else {
                reqwest::Url::parse(&format!("https://{proxy_endpoint}")).ok()?
            };

        let scheme = parsed.scheme();
        let host = parsed.host_str()?.trim();
        if host.is_empty() {
            return None;
        }

        let normalized_host = host.strip_prefix("proxy.").unwrap_or(host);
        Some(format!("{scheme}://api.{normalized_host}"))
    }

    fn resolve_copilot_api_endpoint(token: &str, endpoints: Option<&ApiEndpoints>) -> String {
        endpoints
            .and_then(|entry| entry.api.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .or_else(|| Self::derive_copilot_api_base_url_from_token(token))
            .unwrap_or_else(|| DEFAULT_API.to_string())
    }

    /// Determine Anthropic max tokens for Copilot requests.
    /// Allows overriding via ZEROCLAW_ANTHROPIC_MAX_TOKENS env var; falls back to 4096.
    fn anthropic_max_tokens() -> u32 {
        std::env::var("ZEROCLAW_ANTHROPIC_MAX_TOKENS")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(4096)
    }

    /// Get a GitHub access token from config, cache, or device flow.
    async fn get_github_access_token(&self) -> anyhow::Result<String> {
        if let Some(token) = &self.github_token {
            return Ok(token.clone());
        }

        if let Some(token) = self
            .auth
            .get_provider_bearer_token("github-copilot", self.auth_profile_override.as_deref())
            .await?
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty())
        {
            return Ok(token);
        }

        let access_token_path = self.token_dir.join("access-token");
        if let Ok(cached) = tokio::fs::read_to_string(&access_token_path).await {
            let token = cached.trim();
            if !token.is_empty() {
                return Ok(token.to_string());
            }
        }

        if !std::io::stdin().is_terminal() {
            anyhow::bail!(
                "GitHub Copilot requires interactive device login. Run `zeroclaw models auth login-github-copilot` first."
            );
        }

        let token = self.device_code_login().await?;

        let profile_name = self.profile_name_for_store();
        if let Err(err) = self
            .auth
            .store_provider_token(
                "github-copilot",
                &profile_name,
                &token,
                HashMap::new(),
                true,
            )
            .await
        {
            warn!(
                "Failed to store GitHub Copilot auth profile (github-copilot:{profile_name}): {err}"
            );
        }

        write_file_secure(&access_token_path, &token).await;
        Ok(token)
    }

    /// Run GitHub OAuth device code flow.
    async fn device_code_login(&self) -> anyhow::Result<String> {
        let client = self.http_client();
        let response =
            auth::github_copilot_oauth::request_device_code(&client, "read:user").await?;

        eprintln!(
            "\nGitHub Copilot authentication is required.\n\
             Visit: {}\n\
             Code: {}\n\
             Waiting for authorization...\n",
            response.verification_uri, response.user_code
        );

        let token = auth::github_copilot_oauth::poll_for_access_token(&client, &response).await?;
        eprintln!("Authentication succeeded.\n");
        Ok(token)
    }

    /// Exchange a GitHub access token for a Copilot API key.
    async fn exchange_for_api_key(&self, access_token: &str) -> anyhow::Result<ApiKeyInfo> {
        let request = self
            .http_client()
            .get(GITHUB_API_KEY_URL)
            .header("Accept", "application/json")
            .header("Authorization", format!("token {access_token}"));

        let request = Self::apply_copilot_headers(request);

        let response = request.send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let sanitized = super::sanitize_api_error(&body);

            if status.as_u16() == 401 || status.as_u16() == 403 {
                let access_token_path = self.token_dir.join("access-token");
                tokio::fs::remove_file(&access_token_path).await.ok();
                let profile_name = self.profile_name_for_store();
                let _ = self
                    .auth
                    .remove_profile("github-copilot", &profile_name)
                    .await;
            }

            anyhow::bail!(
                "Failed to get Copilot API key ({status}): {sanitized}. \
                 Ensure your GitHub account has an active Copilot subscription."
            );
        }

        let info: ApiKeyInfo = response.json().await?;
        Ok(info)
    }

    async fn load_api_key_from_disk(&self) -> Option<ApiKeyInfo> {
        let path = self.token_dir.join("api-key.json");
        let data = tokio::fs::read_to_string(&path).await.ok()?;
        serde_json::from_str(&data).ok()
    }

    async fn save_api_key_to_disk(&self, info: &ApiKeyInfo) {
        let path = self.token_dir.join("api-key.json");
        if let Ok(json) = serde_json::to_string_pretty(info) {
            write_file_secure(&path, &json).await;
        }
    }
}

/// Write a file with 0600 permissions (owner read/write only).
/// Uses `spawn_blocking` to avoid blocking the async runtime.
async fn write_file_secure(path: &Path, content: &str) {
    let path = path.to_path_buf();
    let content = content.to_string();

    let result = tokio::task::spawn_blocking(move || {
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)?;
            file.write_all(content.as_bytes())?;

            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            Ok::<(), std::io::Error>(())
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&path, &content)?;
            Ok::<(), std::io::Error>(())
        }
    })
    .await;

    match result {
        Ok(Ok(())) => {}
        Ok(Err(err)) => warn!("Failed to write secure file: {err}"),
        Err(err) => warn!("Failed to spawn blocking write: {err}"),
    }
}

#[async_trait]
impl Provider for CopilotProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let mut messages: Vec<ChatMessage> = Vec::new();
        if let Some(system) = system_prompt {
            messages.push(ChatMessage::system(system));
        }
        messages.push(ChatMessage::user(message));

        let response = self
            .send_chat_request(&messages, None, model, temperature)
            .await?;
        Ok(response.text.unwrap_or_default())
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let response = self
            .send_chat_request(messages, None, model, temperature)
            .await?;
        Ok(response.text.unwrap_or_default())
    }

    async fn chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ProviderChatResponse> {
        self.send_chat_request(request.messages, request.tools, model, temperature)
            .await
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        let _ = self.get_api_key().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(github_token: Option<&str>) -> CopilotProvider {
        CopilotProvider::new(&ProviderRuntimeOptions::default(), github_token)
    }

    #[test]
    fn new_without_token() {
        let provider = provider(None);
        assert!(provider.github_token.is_none());
    }

    #[test]
    fn new_with_token() {
        let provider = provider(Some("ghp_test"));
        assert_eq!(provider.github_token.as_deref(), Some("ghp_test"));
    }

    #[test]
    fn empty_token_treated_as_none() {
        let provider = provider(Some(""));
        assert!(provider.github_token.is_none());
    }

    #[tokio::test]
    async fn cache_starts_empty() {
        let provider = provider(None);
        let cached = provider.refresh_lock.lock().await;
        assert!(cached.is_none());
    }

    #[test]
    fn copilot_headers_include_required_fields() {
        let headers = CopilotProvider::COPILOT_HEADERS;
        assert!(
            headers
                .iter()
                .any(|(header, _)| *header == "Editor-Version")
        );
        assert!(headers.iter().any(|(header, _)| *header == "User-Agent"));
        assert!(
            headers
                .iter()
                .any(|(header, _)| *header == "X-Github-Api-Version")
        );
    }

    #[test]
    fn resolves_transport_api_from_model_id() {
        assert_eq!(
            resolve_copilot_transport_api("claude-sonnet-4.6", "claude-sonnet-4.6"),
            CopilotTransportApi::AnthropicMessages
        );
        assert_eq!(
            resolve_copilot_transport_api("gpt-4o", "gpt-4o"),
            CopilotTransportApi::OpenAiResponses
        );
        assert_eq!(
            resolve_copilot_transport_api("github-copilot/claude-sonnet-4.6", "claude-sonnet-4.6"),
            CopilotTransportApi::AnthropicMessages
        );
    }

    #[test]
    fn supports_native_tools() {
        let provider = provider(None);
        assert!(provider.supports_native_tools());
    }

    #[test]
    fn normalize_model_id_strips_provider_prefix() {
        assert_eq!(normalize_model_id("github-copilot/gpt-4o"), "gpt-4o");
        assert_eq!(normalize_model_id("gpt-4.1"), "gpt-4.1");
    }

    #[test]
    fn derive_base_url_from_proxy_token_hint() {
        let derived = CopilotProvider::derive_copilot_api_base_url_from_token(
            "token;proxy-ep=proxy.example.com;",
        );
        assert_eq!(derived.as_deref(), Some("https://api.example.com"));

        let derived = CopilotProvider::derive_copilot_api_base_url_from_token(
            "token;proxy-ep=https://proxy.foo.bar;",
        );
        assert_eq!(derived.as_deref(), Some("https://api.foo.bar"));
    }

    #[test]
    fn api_response_parses_usage() {
        let json = r#"{
            "choices": [{"message": {"content": "Hello"}}],
            "usage": {"prompt_tokens": 200, "completion_tokens": 80}
        }"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage.unwrap();
        assert_eq!(usage.prompt_tokens, Some(200));
        assert_eq!(usage.completion_tokens, Some(80));
    }

    #[test]
    fn api_response_parses_without_usage() {
        let json = r#"{"choices": [{"message": {"content": "Hello"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.usage.is_none());
    }

    #[test]
    fn to_api_content_user_with_image_returns_parts() {
        let content = "describe this [IMAGE:data:image/png;base64,abc123]";
        let result = CopilotProvider::to_api_content("user", content).unwrap();
        match result {
            ApiContent::Parts(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(&parts[0], ContentPart::Text { text } if text == "describe this"));
                assert!(
                    matches!(&parts[1], ContentPart::ImageUrl { image_url } if image_url.url == "data:image/png;base64,abc123")
                );
            }
            ApiContent::Text(_) => {
                panic!("expected ApiContent::Parts for user message with image marker")
            }
        }
    }

    #[test]
    fn to_api_content_user_plain_returns_text() {
        let result = CopilotProvider::to_api_content("user", "hello world").unwrap();
        assert!(matches!(result, ApiContent::Text(ref s) if s == "hello world"));
    }

    #[test]
    fn to_api_content_non_user_returns_text() {
        let result = CopilotProvider::to_api_content("system", "you are helpful").unwrap();
        assert!(matches!(result, ApiContent::Text(ref s) if s == "you are helpful"));

        let result = CopilotProvider::to_api_content("assistant", "sure").unwrap();
        assert!(matches!(result, ApiContent::Text(ref s) if s == "sure"));
    }

    #[tokio::test]
    async fn convert_messages_for_anthropic_hoists_system_and_merges_roles() {
        let messages = vec![
            ChatMessage::system("system prompt"),
            ChatMessage::system("ignored later system prompt"),
            ChatMessage::user("first user turn"),
            ChatMessage::user("second user turn"),
            ChatMessage::assistant("assistant turn"),
            ChatMessage::tool(r#"{"tool_call_id":"tool-123","content":"tool output"}"#),
        ];

        let (system, native_messages) =
            CopilotProvider::convert_messages_for_anthropic(&messages).await;

        let system = system.expect("system prompt should be hoisted");
        assert_eq!(system.len(), 1);
        assert_eq!(system[0].block_type, "text");
        assert_eq!(system[0].text, "system prompt");
        assert_eq!(system[0].cache_control.cache_type, "ephemeral");

        assert_eq!(native_messages.len(), 3);
        assert_eq!(native_messages[0].role, "user");
        assert_eq!(native_messages[0].content.len(), 2);
        assert!(
            matches!(&native_messages[0].content[0], CopilotAnthropicContentOut::Text { text, cache_control } if text == "first user turn" && cache_control.is_none())
        );
        assert!(
            matches!(&native_messages[0].content[1], CopilotAnthropicContentOut::Text { text, cache_control } if text == "second user turn" && cache_control.is_none())
        );

        assert_eq!(native_messages[1].role, "assistant");
        assert_eq!(native_messages[1].content.len(), 1);
        assert!(
            matches!(&native_messages[1].content[0], CopilotAnthropicContentOut::Text { text, cache_control } if text == "assistant turn" && cache_control.is_none())
        );

        assert_eq!(native_messages[2].role, "user");
        assert_eq!(native_messages[2].content.len(), 1);
        assert!(
            matches!(&native_messages[2].content[0], CopilotAnthropicContentOut::ToolResult { tool_use_id, content } if tool_use_id == "tool-123" && content == "tool output")
        );
    }

    #[tokio::test]
    async fn convert_messages_for_anthropic_preserves_assistant_tool_calls() {
        let messages = vec![ChatMessage::assistant(
            r#"{"content":"assistant summary","tool_calls":[{"id":"call_1","name":"search","arguments":"{\"query\":\"rust\"}"}]}"#,
        )];

        let (_system, native_messages) =
            CopilotProvider::convert_messages_for_anthropic(&messages).await;

        assert_eq!(native_messages.len(), 1);
        assert_eq!(native_messages[0].role, "assistant");
        assert_eq!(native_messages[0].content.len(), 2);
        assert!(
            matches!(&native_messages[0].content[0], CopilotAnthropicContentOut::Text { text, cache_control } if text == "assistant summary" && cache_control.is_none())
        );
        assert!(
            matches!(&native_messages[0].content[1], CopilotAnthropicContentOut::ToolUse { id, name, input } if id == "call_1" && name == "search" && input == &serde_json::json!({"query": "rust"}))
        );
    }

    #[tokio::test]
    async fn anthropic_user_content_data_url() {
        let content = "Here is an image [IMAGE:data:image/png;base64,AA==]";
        let messages = vec![ChatMessage::user(content)];
        let (_system, native_messages) =
            CopilotProvider::convert_messages_for_anthropic(&messages).await;
        assert_eq!(native_messages.len(), 1);
        let first = &native_messages[0];
        assert_eq!(first.role, "user");
        let contains_image = first
            .content
            .iter()
            .any(|c| matches!(c, CopilotAnthropicContentOut::Image { .. }));
        assert!(
            contains_image,
            "expected an Image block in anthropic_user_content"
        );
    }
}
