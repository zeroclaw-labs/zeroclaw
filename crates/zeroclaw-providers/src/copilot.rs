//! GitHub Copilot model_provider with OAuth device-flow authentication.

use crate::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    ModelProvider, TokenUsage, ToolCall as ProviderToolCall,
};
use async_trait::async_trait;
#[cfg(unix)]
use cap_fs_ext::OpenOptionsSyncExt;
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
#[cfg(unix)]
use cap_std::fs::Permissions;
use cap_std::fs::{Dir, File, OpenOptions};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use zeroclaw_api::tool::ToolSpec;

/// GitHub OAuth client ID for Copilot (VS Code extension).
const GITHUB_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const GITHUB_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const GITHUB_ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const GITHUB_API_KEY_URL: &str = "https://api.github.com/copilot_internal/v2/token";
const DEFAULT_API: &str = "https://api.githubcopilot.com";
const CACHE_OPERATION_ADMIT_DIRECTORY: &str = "admit_cache_directory";
const CACHE_OPERATION_PERSIST_ACCESS_TOKEN: &str = "persist_access_token";
const CACHE_OPERATION_PERSIST_API_KEY: &str = "persist_api_key";

// ── Token types ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default = "default_interval")]
    interval: u64,
    #[serde(default = "default_expires_in")]
    expires_in: u64,
}

fn default_interval() -> u64 {
    5
}

fn default_expires_in() -> u64 {
    900
}

#[derive(Debug, Deserialize)]
struct AccessTokenResponse {
    access_token: Option<String>,
    error: Option<String>,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheFailure {
    LocationUnavailable,
    Io {
        kind: io::ErrorKind,
        raw_os_error: Option<i32>,
    },
    BlockingTask,
}

impl CacheFailure {
    fn from_io(error: &io::Error) -> Self {
        Self::Io {
            kind: error.kind(),
            raw_os_error: error.raw_os_error(),
        }
    }
}

fn cache_failure_attrs(operation: &'static str, failure: CacheFailure) -> serde_json::Value {
    match failure {
        CacheFailure::LocationUnavailable => serde_json::json!({
            "operation": operation,
            "error_category": "location_unavailable",
        }),
        CacheFailure::Io { kind, raw_os_error } => serde_json::json!({
            "operation": operation,
            "error_category": "io",
            "error_kind": format!("{kind:?}"),
            "raw_os_error": raw_os_error,
        }),
        CacheFailure::BlockingTask => serde_json::json!({
            "operation": operation,
            "error_category": "blocking_task",
        }),
    }
}

fn warn_cache_failure(operation: &'static str, failure: CacheFailure) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(cache_failure_attrs(operation, failure)),
        "Copilot credential cache operation failed; continuing without persisted credentials"
    );
}

fn cache_result_or_report<T, F>(
    operation: &'static str,
    result: Result<T, CacheFailure>,
    reporter: F,
) -> Option<T>
where
    F: FnOnce(&'static str, CacheFailure),
{
    match result {
        Ok(value) => Some(value),
        Err(failure) => {
            reporter(operation, failure);
            None
        }
    }
}

fn cache_result_or_warn<T>(operation: &'static str, result: Result<T, CacheFailure>) -> Option<T> {
    cache_result_or_report(operation, result, warn_cache_failure)
}

// ── Chat completions types ───────────────────────────────────────

#[derive(Debug, Serialize)]
struct ApiChatRequest<'a> {
    model: String,
    messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
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

// ── ModelProvider ─────────────────────────────────────────────────────

pub struct CopilotModelProvider {
    /// `[providers.models.<family>.<alias>]` config-key alias.
    alias: String,
    github_token: Option<String>,
    /// Mutex ensures only one caller refreshes tokens at a time,
    /// preventing duplicate device flow prompts or redundant API calls.
    refresh_lock: Arc<Mutex<Option<CachedApiKey>>>,
    token_dir: Option<Arc<Dir>>,
}

/// Typed builder for [`CopilotModelProvider`].
///
/// Only `alias` is required. The GitHub token is optional at build time;
/// when unset, the provider will run the device-flow OAuth prompt on
/// first use.
#[must_use]
pub struct CopilotBuilder {
    alias: String,
    github_token: Option<String>,
}

impl CopilotBuilder {
    /// Set an explicit GitHub OAuth token. Empty strings are treated
    /// as missing.
    pub fn github_token(mut self, token: Option<&str>) -> Self {
        self.github_token = token.filter(|t| !t.is_empty()).map(String::from);
        self
    }

    pub fn build(self) -> CopilotModelProvider {
        CopilotModelProvider::new_impl(self.alias, self.github_token)
    }
}

impl CopilotModelProvider {
    /// Entry point. Only `alias` is required; every other field is set
    /// via a labelled chain method on the returned [`CopilotBuilder`].
    pub fn builder(alias: &str) -> CopilotBuilder {
        CopilotBuilder {
            alias: alias.to_string(),
            github_token: None,
        }
    }

    fn new_impl(alias: String, github_token: Option<String>) -> Self {
        let token_dir = cache_result_or_warn(
            CACHE_OPERATION_ADMIT_DIRECTORY,
            cache_dir_from_location(project_cache_dir()),
        );

        Self {
            alias,
            github_token,
            refresh_lock: Arc::new(Mutex::new(None)),
            token_dir,
        }
    }
    fn http_client(&self) -> Client {
        zeroclaw_config::schema::build_runtime_proxy_client_with_timeouts(
            "model_provider.copilot",
            120,
            10,
        )
    }

    /// Required headers for Copilot API requests (editor identification).
    const COPILOT_HEADERS: [(&str, &str); 4] = [
        ("Editor-Version", "vscode/1.85.1"),
        ("Editor-Plugin-Version", "copilot/1.155.0"),
        ("User-Agent", "GithubCopilot/1.155.0"),
        ("Accept", "application/json"),
    ];

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
                if message.role == "assistant"
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content)
                    && let Some(tool_calls_value) = value.get("tool_calls")
                    && let Ok(parsed_calls) =
                        serde_json::from_value::<Vec<ProviderToolCall>>(tool_calls_value.clone())
                {
                    let tool_calls = parsed_calls
                        .into_iter()
                        .map(|tool_call| {
                            let name = tool_call.name;
                            NativeToolCall {
                                id: Some(tool_call.id),
                                kind: Some("function".to_string()),
                                function: NativeFunctionCall {
                                    arguments: crate::compatible::sanitize_tool_arguments(
                                        &name,
                                        &tool_call.arguments,
                                    ),
                                    name,
                                },
                            }
                        })
                        .collect::<Vec<_>>();

                    let content = crate::request_payload::non_empty_string_field(&value, "content")
                        .map(ApiContent::Text);

                    return ApiMessage {
                        role: "assistant".to_string(),
                        content,
                        tool_call_id: None,
                        tool_calls: Some(tool_calls),
                    };
                }

                if message.role == "tool"
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content)
                {
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

                ApiMessage {
                    role: message.role.clone(),
                    content: Self::to_api_content(&message.role, &message.content),
                    tool_call_id: None,
                    tool_calls: None,
                }
            })
            .collect()
    }

    /// Send a chat completions request with required Copilot headers.
    async fn send_chat_request(
        &self,
        messages: Vec<ApiMessage>,
        tools: Option<&[ToolSpec]>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ProviderChatResponse> {
        let (token, endpoint) = self.get_api_key().await?;
        let url = format!("{}/chat/completions", endpoint.trim_end_matches('/'));

        let native_tools = Self::convert_tools(tools);
        let request = ApiChatRequest {
            model: model.to_string(),
            messages,
            temperature,
            // Omit tool_choice when the tool list is empty — spec-compliant
            // validators reject tool_choice without a non-empty tools field.
            tool_choice: native_tools
                .as_ref()
                .and_then(|t| (!t.is_empty()).then(|| "auto".to_string())),
            tools: native_tools,
        };

        let mut req = self
            .http_client()
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&request);

        for (header, value) in &Self::COPILOT_HEADERS {
            req = req.header(*header, *value);
        }

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
        let choice = api_response.choices.into_iter().next().ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                "copilot: empty choices in response"
            );
            anyhow::Error::msg("No response from GitHub Copilot")
        })?;

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
                extra_content: None,
            })
            .collect();

        Ok(ProviderChatResponse {
            text: choice.message.content,
            tool_calls,
            usage,
            reasoning_content: None,
        })
    }

    /// Get a valid Copilot API key, refreshing or re-authenticating as needed.
    /// Uses a Mutex to ensure only one caller refreshes at a time.
    async fn get_api_key(&self) -> anyhow::Result<(String, String)> {
        let mut cached = self.refresh_lock.lock().await;

        if let Some(cached_key) = cached.as_ref()
            && chrono::Utc::now().timestamp() + 120 < cached_key.expires_at
        {
            return Ok((cached_key.token.clone(), cached_key.api_endpoint.clone()));
        }

        if let Some(info) = self.load_api_key_from_disk().await
            && chrono::Utc::now().timestamp() + 120 < info.expires_at
        {
            let endpoint = info
                .endpoints
                .as_ref()
                .and_then(|e| e.api.clone())
                .unwrap_or_else(|| DEFAULT_API.to_string());
            let token = info.token;

            *cached = Some(CachedApiKey {
                token: token.clone(),
                api_endpoint: endpoint.clone(),
                expires_at: info.expires_at,
            });
            return Ok((token, endpoint));
        }

        let access_token = self.get_github_access_token().await?;
        let api_key_info = self.exchange_for_api_key(&access_token).await?;
        self.save_api_key_to_disk(&api_key_info).await;

        let endpoint = api_key_info
            .endpoints
            .as_ref()
            .and_then(|e| e.api.clone())
            .unwrap_or_else(|| DEFAULT_API.to_string());

        *cached = Some(CachedApiKey {
            token: api_key_info.token.clone(),
            api_endpoint: endpoint.clone(),
            expires_at: api_key_info.expires_at,
        });

        Ok((api_key_info.token, endpoint))
    }

    /// Get a GitHub access token from config, cache, or device flow.
    async fn get_github_access_token(&self) -> anyhow::Result<String> {
        if let Some(token) = &self.github_token {
            return Ok(token.clone());
        }

        if let Some(token_dir) = self.token_dir.as_ref()
            && let Some(cached) = read_cache_file(token_dir, "access-token").await
        {
            let token = cached.trim();
            if !token.is_empty() {
                return Ok(token.to_string());
            }
        }

        let token = self.device_code_login().await?;
        if let Some(token_dir) = self.token_dir.as_ref() {
            let _ = cache_result_or_warn(
                CACHE_OPERATION_PERSIST_ACCESS_TOKEN,
                write_file_secure(token_dir, "access-token", &token).await,
            );
        }
        Ok(token)
    }

    /// Run GitHub OAuth device code flow.
    async fn device_code_login(&self) -> anyhow::Result<String> {
        let response: DeviceCodeResponse = self
            .http_client()
            .post(GITHUB_DEVICE_CODE_URL)
            .header("Accept", "application/json")
            .json(&serde_json::json!({
                "client_id": GITHUB_CLIENT_ID,
                "scope": "read:user"
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let mut poll_interval = Duration::from_secs(response.interval.max(5));
        let expires_in = response.expires_in.max(1);
        let expires_at = tokio::time::Instant::now() + Duration::from_secs(expires_in);

        eprintln!(
            "\nGitHub Copilot authentication is required.\n\
             Visit: {}\n\
             Code: {}\n\
             Waiting for authorization...\n",
            response.verification_uri, response.user_code
        );

        while tokio::time::Instant::now() < expires_at {
            tokio::time::sleep(poll_interval).await;

            let token_response: AccessTokenResponse = self
                .http_client()
                .post(GITHUB_ACCESS_TOKEN_URL)
                .header("Accept", "application/json")
                .json(&serde_json::json!({
                    "client_id": GITHUB_CLIENT_ID,
                    "device_code": response.device_code,
                    "grant_type": "urn:ietf:params:oauth:grant-type:device_code"
                }))
                .send()
                .await?
                .json()
                .await?;

            if let Some(token) = token_response.access_token {
                eprintln!("Authentication succeeded.\n");
                return Ok(token);
            }

            match token_response.error.as_deref() {
                Some("slow_down") => {
                    poll_interval += Duration::from_secs(5);
                }
                Some("authorization_pending") | None => {}
                Some("expired_token") => {
                    anyhow::bail!("GitHub device authorization expired")
                }
                Some(error) => anyhow::bail!("GitHub auth failed: {error}"),
            }
        }

        anyhow::bail!("Timed out waiting for GitHub authorization")
    }

    /// Exchange a GitHub access token for a Copilot API key.
    async fn exchange_for_api_key(&self, access_token: &str) -> anyhow::Result<ApiKeyInfo> {
        let mut request = self.http_client().get(GITHUB_API_KEY_URL);
        for (header, value) in &Self::COPILOT_HEADERS {
            request = request.header(*header, *value);
        }
        request = request.header("Authorization", format!("token {access_token}"));

        let response = request.send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let sanitized = super::sanitize_api_error(&body);

            if (status.as_u16() == 401 || status.as_u16() == 403)
                && let Some(token_dir) = self.token_dir.as_ref()
            {
                remove_cache_file(token_dir, "access-token").await;
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
        let token_dir = self.token_dir.as_ref()?;
        let data = read_cache_file(token_dir, "api-key.json").await?;
        serde_json::from_str(&data).ok()
    }

    async fn save_api_key_to_disk(&self, info: &ApiKeyInfo) {
        if let Some(token_dir) = self.token_dir.as_ref()
            && let Ok(json) = serde_json::to_string_pretty(info)
        {
            let _ = cache_result_or_warn(
                CACHE_OPERATION_PERSIST_API_KEY,
                write_file_secure(token_dir, "api-key.json", &json).await,
            );
        }
    }
}

fn project_cache_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "zeroclaw").map(|dir| dir.config_dir().join("copilot"))
}

fn cache_dir_from_location(path: Option<PathBuf>) -> Result<Arc<Dir>, CacheFailure> {
    let path = path.ok_or(CacheFailure::LocationUnavailable)?;
    admit_cache_dir(&path).map_err(|error| CacheFailure::from_io(&error))
}

fn admit_cache_dir(path: &Path) -> io::Result<Arc<Dir>> {
    let parent_path = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "cache path has no parent"))?;
    let leaf = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "cache path has no leaf"))?;
    fs::create_dir_all(parent_path)?;

    let parent = Dir::open_ambient_dir(parent_path, cap_std::ambient_authority())?;
    match parent.symlink_metadata(leaf) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cache directory entry is not a directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = cap_std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use cap_std::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match parent.create_dir_with(leaf, &builder) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    }

    let cache_dir = parent.open_dir_nofollow(leaf)?;
    #[cfg(unix)]
    {
        use cap_std::fs::PermissionsExt;
        cache_dir.set_permissions(".", Permissions::from_mode(0o700))?;
    }
    Ok(Arc::new(cache_dir))
}

fn ensure_final_cache_entry(dir: &Dir, name: &str) -> io::Result<()> {
    match dir.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cache entry is not a regular file",
            ))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn read_cache_file_sync(dir: &Dir, name: &str) -> io::Result<String> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    #[cfg(unix)]
    options.nonblock(true);
    let mut file = dir.open_with(name, &options)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cache entry is not a regular file",
        ));
    }
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    Ok(content)
}

async fn read_cache_file(dir: &Arc<Dir>, name: &'static str) -> Option<String> {
    let dir = Arc::clone(dir);
    tokio::task::spawn_blocking(move || read_cache_file_sync(&dir, name))
        .await
        .ok()
        .and_then(Result::ok)
}

fn temp_cache_name(final_name: &str) -> String {
    format!(".{final_name}.tmp-{}", uuid::Uuid::new_v4())
}

fn create_temp_cache_file_with<F>(
    dir: &Dir,
    final_name: &str,
    mut name_factory: F,
) -> io::Result<(File, String)>
where
    F: FnMut(&str) -> String,
{
    for _ in 0..8 {
        let name = name_factory(final_name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        match dir.open_with(&name, &options) {
            Ok(file) => return Ok((file, name)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "unable to allocate a unique Copilot cache temporary file",
    ))
}

struct TempCacheFileGuard {
    dir: Arc<Dir>,
    name: Option<String>,
}

impl Drop for TempCacheFileGuard {
    fn drop(&mut self) {
        if let Some(name) = self.name.take() {
            let _ = self.dir.remove_file(name);
        }
    }
}

fn write_cache_file_sync(dir: &Arc<Dir>, final_name: &str, content: &str) -> io::Result<()> {
    ensure_final_cache_entry(dir, final_name)?;

    let (mut file, temp_name) = create_temp_cache_file_with(dir, final_name, temp_cache_name)?;
    let mut guard = TempCacheFileGuard {
        dir: Arc::clone(dir),
        name: Some(temp_name.clone()),
    };

    #[cfg(unix)]
    {
        use cap_std::fs::PermissionsExt;
        file.set_permissions(Permissions::from_mode(0o600))?;
    }

    file.write_all(content.as_bytes())?;
    file.flush()?;
    file.sync_all()?;
    drop(file);

    ensure_final_cache_entry(dir, final_name)?;
    dir.rename(&temp_name, dir, final_name)?;
    guard.name = None;
    Ok(())
}

async fn write_file_secure(
    dir: &Arc<Dir>,
    name: &'static str,
    content: &str,
) -> Result<(), CacheFailure> {
    let dir = Arc::clone(dir);
    let content = content.to_string();
    match tokio::task::spawn_blocking(move || write_cache_file_sync(&dir, name, &content)).await {
        Ok(result) => result.map_err(|error| CacheFailure::from_io(&error)),
        Err(_) => Err(CacheFailure::BlockingTask),
    }
}

fn remove_cache_file_sync(dir: &Dir, name: &str) -> io::Result<()> {
    let metadata = dir.symlink_metadata(name)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cache entry is not a regular file",
        ));
    }
    dir.remove_file(name)
}

async fn remove_cache_file(dir: &Arc<Dir>, name: &'static str) -> bool {
    let dir = Arc::clone(dir);
    tokio::task::spawn_blocking(move || remove_cache_file_sync(&dir, name))
        .await
        .is_ok_and(|result| result.is_ok())
}

#[async_trait]
impl ModelProvider for CopilotModelProvider {
    // ── ModelProvider-family defaults ──
    fn default_base_url(&self) -> Option<&str> {
        Some(DEFAULT_API)
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let mut messages = Vec::new();
        if let Some(system) = system_prompt {
            messages.push(ApiMessage {
                role: "system".to_string(),
                content: Some(ApiContent::Text(system.to_string())),
                tool_call_id: None,
                tool_calls: None,
            });
        }
        messages.push(ApiMessage {
            role: "user".to_string(),
            content: Self::to_api_content("user", message),
            tool_call_id: None,
            tool_calls: None,
        });

        let response = self
            .send_chat_request(messages, None, model, temperature)
            .await?;
        Ok(response.text.unwrap_or_default())
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let response = self
            .send_chat_request(Self::convert_messages(messages), None, model, temperature)
            .await?;
        Ok(response.text.unwrap_or_default())
    }

    async fn chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ProviderChatResponse> {
        self.send_chat_request(
            Self::convert_messages(request.messages),
            request.tools,
            model,
            temperature,
        )
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

impl ::zeroclaw_api::attribution::Attributable for CopilotModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Copilot,
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

    #[test]
    fn new_without_token() {
        let model_provider = CopilotModelProvider::builder("test")
            .github_token(None)
            .build();
        assert!(model_provider.github_token.is_none());
    }

    #[test]
    fn new_with_token() {
        let model_provider = CopilotModelProvider::builder("test")
            .github_token(Some("ghp_test"))
            .build();
        assert_eq!(model_provider.github_token.as_deref(), Some("ghp_test"));
    }

    #[test]
    fn empty_token_treated_as_none() {
        let model_provider = CopilotModelProvider::builder("test")
            .github_token(Some(""))
            .build();
        assert!(model_provider.github_token.is_none());
    }

    #[tokio::test]
    async fn cache_starts_empty() {
        let model_provider = CopilotModelProvider::builder("test")
            .github_token(None)
            .build();
        let cached = model_provider.refresh_lock.lock().await;
        assert!(cached.is_none());
    }

    #[test]
    fn copilot_headers_include_required_fields() {
        let headers = CopilotModelProvider::COPILOT_HEADERS;
        assert!(
            headers
                .iter()
                .any(|(header, _)| *header == "Editor-Version")
        );
        assert!(
            headers
                .iter()
                .any(|(header, _)| *header == "Editor-Plugin-Version")
        );
        assert!(headers.iter().any(|(header, _)| *header == "User-Agent"));
    }

    #[test]
    fn default_interval_and_expiry() {
        assert_eq!(default_interval(), 5);
        assert_eq!(default_expires_in(), 900);
    }

    #[test]
    fn supports_native_tools() {
        let model_provider = CopilotModelProvider::builder("test")
            .github_token(None)
            .build();
        assert!(model_provider.supports_native_tools());
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
        let result = CopilotModelProvider::to_api_content("user", content).unwrap();
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
        let result = CopilotModelProvider::to_api_content("user", "hello world").unwrap();
        assert!(matches!(result, ApiContent::Text(ref s) if s == "hello world"));
    }

    #[test]
    fn to_api_content_non_user_returns_text() {
        let result = CopilotModelProvider::to_api_content("system", "you are helpful").unwrap();
        assert!(matches!(result, ApiContent::Text(ref s) if s == "you are helpful"));

        let result = CopilotModelProvider::to_api_content("assistant", "sure").unwrap();
        assert!(matches!(result, ApiContent::Text(ref s) if s == "sure"));
    }

    #[test]
    fn convert_messages_sanitizes_invalid_tool_arguments_to_empty_object() {
        // Pins that the copilot `convert_messages` call site of
        // `sanitize_tool_arguments` is wired in. The helper contract itself is
        // covered in `compatible::tests::sanitize_tool_arguments_*`.
        use zeroclaw_api::model_provider::ChatMessage;

        let messages = vec![ChatMessage {
            role: "assistant".into(),
            content: r#"{"content":"trying","tool_calls":[{"id":"call_bad","name":"shell","arguments":"{\"command\":\"rm -rf"}]}"#
                .into(),
        }];

        let api_messages = CopilotModelProvider::convert_messages(&messages);
        assert_eq!(api_messages.len(), 1);
        let tool_calls = api_messages[0].tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id.as_deref(), Some("call_bad"));
        assert_eq!(tool_calls[0].function.name, "shell");
        assert_eq!(tool_calls[0].function.arguments, "{}");
    }

    #[test]
    fn convert_messages_passes_through_valid_tool_arguments() {
        // Companion regression: valid JSON must round-trip byte-for-byte.
        use zeroclaw_api::model_provider::ChatMessage;

        let messages = vec![ChatMessage {
            role: "assistant".into(),
            content: r#"{"content":"using","tool_calls":[{"id":"call_ok","name":"shell","arguments":"{\"command\":\"pwd\"}"}]}"#
                .into(),
        }];

        let api_messages = CopilotModelProvider::convert_messages(&messages);
        let tool_calls = api_messages[0].tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls[0].function.arguments, r#"{"command":"pwd"}"#);
    }

    fn provider_with_cache_dir(dir: Arc<Dir>) -> CopilotModelProvider {
        CopilotModelProvider {
            alias: "test".to_string(),
            github_token: None,
            refresh_lock: Arc::new(Mutex::new(None)),
            token_dir: Some(dir),
        }
    }

    #[test]
    fn unavailable_project_location_disables_cache_without_temp_fallback() {
        let location: Option<PathBuf> = None;
        assert_eq!(
            cache_dir_from_location(location).unwrap_err(),
            CacheFailure::LocationUnavailable
        );
    }

    #[test]
    fn cache_failure_attrs_omit_paths_and_secret_bearing_error_text() {
        let error = io::Error::new(
            io::ErrorKind::PermissionDenied,
            "credential secret at /private/cache/path",
        );
        let attrs = cache_failure_attrs("persist_api_key", CacheFailure::from_io(&error));

        assert_eq!(attrs["operation"], "persist_api_key");
        assert_eq!(attrs["error_category"], "io");
        assert_eq!(attrs["error_kind"], "PermissionDenied");
        let serialized = attrs.to_string();
        assert!(!serialized.contains("credential secret"));
        assert!(!serialized.contains("/private/cache/path"));
    }

    #[test]
    fn blocking_task_failure_has_a_fixed_sanitized_category() {
        let attrs = cache_failure_attrs(
            CACHE_OPERATION_PERSIST_ACCESS_TOKEN,
            CacheFailure::BlockingTask,
        );

        assert_eq!(attrs["operation"], CACHE_OPERATION_PERSIST_ACCESS_TOKEN);
        assert_eq!(attrs["error_category"], "blocking_task");
        assert!(attrs.get("error_kind").is_none());
    }

    #[test]
    fn cache_failure_reporting_is_exactly_once_and_success_is_silent() {
        for operation in [
            CACHE_OPERATION_ADMIT_DIRECTORY,
            CACHE_OPERATION_PERSIST_ACCESS_TOKEN,
            CACHE_OPERATION_PERSIST_API_KEY,
        ] {
            let mut reports = Vec::new();
            let value: Option<()> = cache_result_or_report(
                operation,
                Err(CacheFailure::Io {
                    kind: io::ErrorKind::PermissionDenied,
                    raw_os_error: Some(13),
                }),
                |reported_operation, failure| {
                    reports.push(cache_failure_attrs(reported_operation, failure));
                },
            );

            assert!(value.is_none());
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0]["operation"], operation);
            assert_eq!(reports[0]["error_kind"], "PermissionDenied");
        }

        let mut reported = false;
        let value =
            cache_result_or_report(CACHE_OPERATION_PERSIST_API_KEY, Ok("persisted"), |_, _| {
                reported = true
            });
        assert_eq!(value, Some("persisted"));
        assert!(!reported);
    }

    #[tokio::test]
    async fn cache_round_trip_for_access_and_api_key() {
        let temp = tempfile::tempdir().unwrap();
        let cache_path = temp.path().join("copilot");
        let cache_dir = admit_cache_dir(&cache_path).unwrap();
        let provider = provider_with_cache_dir(Arc::clone(&cache_dir));

        assert!(
            write_file_secure(&cache_dir, "access-token", "gho_round_trip")
                .await
                .is_ok()
        );
        assert_eq!(
            read_cache_file(&cache_dir, "access-token").await.as_deref(),
            Some("gho_round_trip")
        );

        let info = ApiKeyInfo {
            token: "api_round_trip".to_string(),
            expires_at: 4_000_000_000,
            endpoints: Some(ApiEndpoints {
                api: Some("https://api.example.test".to_string()),
            }),
        };
        provider.save_api_key_to_disk(&info).await;
        let loaded = provider.load_api_key_from_disk().await.unwrap();
        assert_eq!(loaded.token, info.token);
        assert_eq!(loaded.expires_at, info.expires_at);
        assert_eq!(
            loaded
                .endpoints
                .as_ref()
                .and_then(|endpoints| endpoints.api.as_deref()),
            info.endpoints
                .as_ref()
                .and_then(|endpoints| endpoints.api.as_deref())
        );
    }

    #[cfg(unix)]
    #[test]
    fn cache_directory_and_file_permissions_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let cache_path = temp.path().join("copilot");
        let cache_dir = admit_cache_dir(&cache_path).unwrap();
        assert_eq!(
            fs::metadata(&cache_path).unwrap().permissions().mode() & 0o777,
            0o700
        );

        assert!(write_cache_file_sync(&cache_dir, "api-key.json", "secret").is_ok());
        assert_eq!(
            fs::metadata(cache_path.join("api-key.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_cache_directory_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = temp.path().join("copilot");
        symlink(&target, &link).unwrap();

        let parent = Dir::open_ambient_dir(temp.path(), cap_std::ambient_authority()).unwrap();
        assert!(parent.open_dir_nofollow("copilot").is_err());
        assert!(admit_cache_dir(&link).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_cache_file_is_rejected_for_read_and_write() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let cache_dir = admit_cache_dir(&temp.path().join("copilot")).unwrap();
        let target = temp.path().join("external");
        fs::write(&target, "external-original").unwrap();
        let cache_file = temp.path().join("copilot/api-key.json");
        symlink(&target, &cache_file).unwrap();

        assert!(read_cache_file(&cache_dir, "api-key.json").await.is_none());
        assert!(
            write_file_secure(&cache_dir, "api-key.json", "must-not-follow")
                .await
                .is_err()
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "external-original");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn visible_directory_swap_cannot_redirect_retained_capability() {
        let temp = tempfile::tempdir().unwrap();
        let visible = temp.path().join("copilot");
        let retained_path = temp.path().join("retained");
        let external = temp.path().join("external");
        fs::create_dir(&visible).unwrap();
        fs::create_dir(&external).unwrap();
        let cache_dir = admit_cache_dir(&visible).unwrap();

        fs::rename(&visible, &retained_path).unwrap();
        fs::write(external.join("api-key.json"), "external-original").unwrap();
        fs::rename(&external, &visible).unwrap();

        assert!(
            write_file_secure(&cache_dir, "api-key.json", "retained-content")
                .await
                .is_ok()
        );
        assert_eq!(
            read_cache_file(&cache_dir, "api-key.json").await.as_deref(),
            Some("retained-content")
        );
        assert_eq!(
            fs::read_to_string(visible.join("api-key.json")).unwrap(),
            "external-original"
        );
        assert!(remove_cache_file(&cache_dir, "api-key.json").await);
        assert!(!retained_path.join("api-key.json").exists());
        assert!(visible.join("api-key.json").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn final_child_link_escape_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let cache_path = temp.path().join("copilot");
        let cache_dir = admit_cache_dir(&cache_path).unwrap();
        let external = temp.path().join("external");
        fs::write(&external, "external-original").unwrap();
        std::os::unix::fs::symlink(&external, cache_path.join("api-key.json")).unwrap();

        assert!(read_cache_file(&cache_dir, "api-key.json").await.is_none());
        assert!(
            write_file_secure(&cache_dir, "api-key.json", "credential")
                .await
                .is_err()
        );
        assert!(!remove_cache_file(&cache_dir, "api-key.json").await);
        assert_eq!(fs::read_to_string(external).unwrap(), "external-original");
    }

    #[tokio::test]
    async fn regular_cache_file_replacement_is_atomic_and_complete() {
        let temp = tempfile::tempdir().unwrap();
        let cache_dir = admit_cache_dir(&temp.path().join("copilot")).unwrap();

        assert!(
            write_file_secure(&cache_dir, "api-key.json", "old-complete-content")
                .await
                .is_ok()
        );
        assert!(
            write_file_secure(&cache_dir, "api-key.json", "new-complete-content")
                .await
                .is_ok()
        );
        assert_eq!(
            read_cache_file(&cache_dir, "api-key.json").await.as_deref(),
            Some("new-complete-content")
        );
    }

    #[tokio::test]
    async fn cache_file_replacement_succeeds_while_old_handle_remains_open() {
        let temp = tempfile::tempdir().unwrap();
        let cache_dir = admit_cache_dir(&temp.path().join("copilot")).unwrap();
        let old_content = "old-complete-content";
        let new_content = "new-complete-content";

        assert!(
            write_file_secure(&cache_dir, "api-key.json", old_content)
                .await
                .is_ok()
        );

        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let mut old_file = cache_dir.open_with("api-key.json", &options).unwrap();

        assert!(
            write_file_secure(&cache_dir, "api-key.json", new_content)
                .await
                .is_ok()
        );

        let mut retained_content = String::new();
        old_file.read_to_string(&mut retained_content).unwrap();
        assert_eq!(retained_content, old_content);
        assert_eq!(
            read_cache_file(&cache_dir, "api-key.json").await.as_deref(),
            Some(new_content)
        );
    }

    #[cfg(unix)]
    #[test]
    fn fifo_cache_file_read_fails_without_blocking() {
        let temp = tempfile::tempdir().unwrap();
        let cache_path = temp.path().join("copilot");
        let cache_dir = admit_cache_dir(&cache_path).unwrap();
        let fifo_path = cache_path.join("api-key.json");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo_path)
            .status()
            .expect("mkfifo should be available on unix test hosts");
        assert!(status.success(), "mkfifo failed");

        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            sender
                .send(read_cache_file_sync(&cache_dir, "api-key.json"))
                .unwrap();
        });
        let result = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("reading a FIFO cache entry must return promptly");
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        worker.join().unwrap();
    }

    #[test]
    fn deterministic_temp_collision_preserves_foreign_entry() {
        let temp = tempfile::tempdir().unwrap();
        let cache_dir = admit_cache_dir(&temp.path().join("copilot")).unwrap();
        let foreign_name = ".api-key.json.tmp-foreign";
        cache_dir.write(foreign_name, "foreign-content").unwrap();

        let result =
            create_temp_cache_file_with(&cache_dir, "api-key.json", |_| foreign_name.to_string());
        match result {
            Err(error) => assert_eq!(error.kind(), io::ErrorKind::AlreadyExists),
            Ok(_) => panic!("foreign temporary entry was unexpectedly replaced"),
        }
        assert_eq!(
            cache_dir.read_to_string(foreign_name).unwrap(),
            "foreign-content"
        );
    }

    #[test]
    fn injected_failure_after_temp_creation_removes_only_owned_temp() {
        let temp = tempfile::tempdir().unwrap();
        let cache_dir = admit_cache_dir(&temp.path().join("copilot")).unwrap();
        let foreign_name = ".api-key.json.tmp-foreign";
        cache_dir.write(foreign_name, "foreign-content").unwrap();

        let (file, owned_name) = create_temp_cache_file_with(&cache_dir, "api-key.json", |_| {
            ".api-key.json.tmp-owned".to_string()
        })
        .unwrap();
        drop(file);
        {
            let _guard = TempCacheFileGuard {
                dir: Arc::clone(&cache_dir),
                name: Some(owned_name.clone()),
            };
            let injected_failure: io::Result<()> = Err(io::Error::other("injected failure"));
            assert!(injected_failure.is_err());
        }
        assert!(cache_dir.symlink_metadata(&owned_name).is_err());
        assert_eq!(
            cache_dir.read_to_string(foreign_name).unwrap(),
            "foreign-content"
        );
    }
}
