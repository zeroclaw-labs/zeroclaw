#![allow(clippy::to_string_in_format_args)]
//! ModelProvider subsystem for model inference backends.
//!
//! This module implements the factory pattern for AI model model_providers. Each model_provider
//! implements the [`ModelProvider`] trait defined in [`traits`], and is registered in the
//! factory function [`create_model_provider`] by its canonical string key (e.g., `"openai"`,
//! `"anthropic"`, `"ollama"`, `"gemini"`). ModelProvider aliases are resolved internally
//! so that user-facing keys remain stable.
//!
//! Each model_provider call goes through the [`ReliableModelProvider`] wrapper, which adds
//! automatic retry with exponential backoff and API-key rotation on rate limits.
//! Model routing across multiple model_providers is available via [`create_routed_model_provider`].
//!
//! # Extension
//!
//! To add a new model_provider, implement [`ModelProvider`] in a new submodule and register it
//! in [`create_model_provider_with_url`]. See `AGENTS.md` §7.1 for the full change playbook.

pub mod anthropic;
pub mod auth;
pub mod azure_openai;
pub mod bedrock;
pub mod catalog;
pub mod compatible;
pub mod copilot;
pub mod factory;
pub mod gemini;
pub mod gemini_cli;
// glm.rs excluded — not compiled in upstream (dead code with known issues)
pub mod kilocli;
pub mod models_dev;
pub mod multimodal;
pub mod ollama;
pub mod openai;
pub mod openai_codex;
pub mod openrouter;
pub mod openrouter_catalog;
pub mod reliable;
pub mod router;
pub mod telnyx;
pub mod traits;

#[allow(unused_imports)]
pub use traits::{
    ChatMessage, ChatRequest, ChatResponse, ConversationMessage, ModelProvider,
    ProviderCapabilityError, ToolCall, ToolResultMessage,
};

use reliable::ReliableModelProvider;
use serde::Deserialize;
use std::path::PathBuf;

const MAX_API_ERROR_CHARS: usize = 500;
const MINIMAX_INTL_BASE_URL: &str = "https://api.minimax.io/v1";
/// MiniMax-published OAuth client_id (same one their portal uses).
/// Operators with a custom OAuth app override via
/// `[model_providers.minimax.<alias>] oauth_client_id = "..."`.
const MINIMAX_OAUTH_DEFAULT_CLIENT_ID: &str = "78257093-7e40-4613-99e0-527b14b39113";
const GLM_GLOBAL_BASE_URL: &str = "https://api.z.ai/api/paas/v4";
const MOONSHOT_INTL_BASE_URL: &str = "https://api.moonshot.ai/v1";
const QWEN_CN_BASE_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1";
const QWEN_OAUTH_BASE_FALLBACK_URL: &str = QWEN_CN_BASE_URL;
const QWEN_OAUTH_TOKEN_ENDPOINT: &str = "https://chat.qwen.ai/api/v1/oauth2/token";
const QWEN_OAUTH_PLACEHOLDER: &str = "qwen-oauth";
const QWEN_OAUTH_DEFAULT_CLIENT_ID: &str = "f0304373b74a44d2b584a3fb70ca9e56";
const QWEN_OAUTH_CREDENTIAL_FILE: &str = ".qwen/oauth_creds.json";
const ZAI_GLOBAL_BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";
const QIANFAN_BASE_URL: &str = "https://qianfan.baidubce.com/v2";
const VERCEL_AI_GATEWAY_BASE_URL: &str = "https://ai-gateway.vercel.sh/v1";

pub fn is_minimax_intl_alias(name: &str) -> bool {
    matches!(
        name,
        "minimax"
            | "minimax-intl"
            | "minimax-io"
            | "minimax-global"
            | "minimax-oauth"
            | "minimax-portal"
            | "minimax-oauth-global"
            | "minimax-portal-global"
    )
}
pub fn is_minimax_cn_alias(name: &str) -> bool {
    matches!(
        name,
        "minimax-cn" | "minimaxi" | "minimax-oauth-cn" | "minimax-portal-cn"
    )
}
pub fn is_minimax_alias(name: &str) -> bool {
    is_minimax_intl_alias(name) || is_minimax_cn_alias(name)
}
pub fn is_glm_global_alias(name: &str) -> bool {
    matches!(name, "glm" | "zhipu" | "glm-global" | "zhipu-global")
}

pub fn is_glm_cn_alias(name: &str) -> bool {
    matches!(name, "glm-cn" | "zhipu-cn" | "bigmodel")
}

pub fn is_glm_alias(name: &str) -> bool {
    is_glm_global_alias(name) || is_glm_cn_alias(name)
}

pub fn is_moonshot_intl_alias(name: &str) -> bool {
    matches!(
        name,
        "moonshot-intl" | "moonshot-global" | "kimi-intl" | "kimi-global"
    )
}

pub fn is_moonshot_cn_alias(name: &str) -> bool {
    matches!(name, "moonshot" | "kimi" | "moonshot-cn" | "kimi-cn")
}

pub fn is_moonshot_alias(name: &str) -> bool {
    is_moonshot_intl_alias(name) || is_moonshot_cn_alias(name)
}

pub fn is_qwen_cn_alias(name: &str) -> bool {
    matches!(name, "qwen" | "dashscope" | "qwen-cn" | "dashscope-cn")
}

pub fn is_qwen_intl_alias(name: &str) -> bool {
    matches!(
        name,
        "qwen-intl" | "dashscope-intl" | "qwen-international" | "dashscope-international"
    )
}

pub fn is_qwen_us_alias(name: &str) -> bool {
    matches!(name, "qwen-us" | "dashscope-us")
}

pub fn is_qwen_oauth_alias(name: &str) -> bool {
    matches!(name, "qwen-code" | "qwen-oauth" | "qwen_oauth")
}

pub fn is_bailian_alias(name: &str) -> bool {
    matches!(name, "bailian" | "aliyun-bailian" | "aliyun")
}

pub fn is_qwen_alias(name: &str) -> bool {
    is_qwen_cn_alias(name)
        || is_qwen_intl_alias(name)
        || is_qwen_us_alias(name)
        || is_qwen_oauth_alias(name)
}

pub fn is_zai_global_alias(name: &str) -> bool {
    matches!(name, "zai" | "z.ai" | "zai-global" | "z.ai-global")
}

pub fn is_zai_cn_alias(name: &str) -> bool {
    matches!(name, "zai-cn" | "z.ai-cn")
}

pub fn is_zai_alias(name: &str) -> bool {
    is_zai_global_alias(name) || is_zai_cn_alias(name)
}

pub fn is_qianfan_alias(name: &str) -> bool {
    matches!(name, "qianfan" | "baidu")
}

fn qianfan_base_url(api_url: Option<&str>) -> String {
    api_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| QIANFAN_BASE_URL.to_string())
}

pub fn is_doubao_alias(name: &str) -> bool {
    matches!(name, "doubao" | "volcengine" | "ark" | "doubao-cn")
}

#[derive(Clone, Deserialize, Default)]
pub(crate) struct QwenOauthCredentials {
    #[serde(default)]
    pub(crate) access_token: Option<String>,
    #[serde(default)]
    pub(crate) refresh_token: Option<String>,
    #[serde(default)]
    pub(crate) resource_url: Option<String>,
    #[serde(default)]
    pub(crate) expiry_date: Option<i64>,
}

impl std::fmt::Debug for QwenOauthCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QwenOauthCredentials")
            .field("resource_url", &self.resource_url)
            .field("expiry_date", &self.expiry_date)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Deserialize)]
struct QwenOauthTokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    resource_url: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

#[derive(Clone, Default)]
pub(crate) struct QwenOauthProviderContext {
    pub(crate) credential: Option<String>,
    pub(crate) base_url: Option<String>,
}

impl std::fmt::Debug for QwenOauthProviderContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QwenOauthProviderContext")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

fn qwen_oauth_client_id() -> String {
    QWEN_OAUTH_DEFAULT_CLIENT_ID.to_string()
}

fn qwen_oauth_credentials_file_path() -> Option<PathBuf> {
    // OS path resolution; not a config override.
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .map(|home| home.join(QWEN_OAUTH_CREDENTIAL_FILE))
}

fn normalize_qwen_oauth_base_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }

    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };

    let normalized = with_scheme.trim_end_matches('/').to_string();
    if normalized.ends_with("/v1") {
        Some(normalized)
    } else {
        Some(format!("{normalized}/v1"))
    }
}

fn read_qwen_oauth_cached_credentials() -> Option<QwenOauthCredentials> {
    let path = qwen_oauth_credentials_file_path()?;
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<QwenOauthCredentials>(&content).ok()
}

fn normalized_qwen_expiry_millis(raw: i64) -> i64 {
    if raw < 10_000_000_000 {
        raw.saturating_mul(1000)
    } else {
        raw
    }
}

fn qwen_oauth_token_expired(credentials: &QwenOauthCredentials) -> bool {
    let Some(expiry) = credentials.expiry_date else {
        return false;
    };

    let expiry_millis = normalized_qwen_expiry_millis(expiry);
    let now_millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(i64::MAX);

    expiry_millis <= now_millis.saturating_add(30_000)
}

pub(crate) fn refresh_qwen_oauth_access_token(
    refresh_token: &str,
    client_id: &str,
) -> anyhow::Result<QwenOauthCredentials> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new());

    let response = client
        .post(QWEN_OAUTH_TOKEN_ENDPOINT)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ])
        .send()
        .map_err(|error| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "oauth_provider": "qwen",
                        "phase": "refresh_request",
                        "error": format!("{}", error),
                    })),
                "qwen: OAuth refresh request failed"
            );
            anyhow::Error::msg(format!("OAuth refresh request failed: {error}"))
        })?;

    let status = response.status();
    let body = response
        .text()
        .unwrap_or_else(|_| "<failed to read Qwen OAuth response body>".to_string());

    let parsed = serde_json::from_str::<QwenOauthTokenResponse>(&body).ok();

    if !status.is_success() {
        let detail = parsed
            .as_ref()
            .and_then(|payload| payload.error_description.as_deref())
            .or_else(|| parsed.as_ref().and_then(|payload| payload.error.as_deref()))
            .filter(|msg| !msg.trim().is_empty())
            .unwrap_or(body.as_str());
        anyhow::bail!("OAuth refresh failed (HTTP {status}): {detail}");
    }

    let payload = parsed.ok_or_else(|| {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "oauth_provider": "qwen",
                    "phase": "refresh_parse",
                })),
            "qwen: OAuth refresh response is not JSON"
        );
        anyhow::Error::msg("OAuth refresh response is not JSON")
    })?;

    if let Some(error_code) = payload
        .error
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let detail = payload.error_description.as_deref().unwrap_or(error_code);
        anyhow::bail!("OAuth refresh failed: {detail}");
    }

    let access_token = payload
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "oauth_provider": "qwen",
                        "field": "access_token",
                    })),
                "qwen: OAuth refresh response missing access_token"
            );
            anyhow::Error::msg("OAuth refresh response missing access_token")
        })?
        .to_string();

    let expiry_date = payload.expires_in.and_then(|seconds| {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_secs()).ok())?;
        now_secs
            .checked_add(seconds)
            .and_then(|unix_secs| unix_secs.checked_mul(1000))
    });

    Ok(QwenOauthCredentials {
        access_token: Some(access_token),
        refresh_token: payload
            .refresh_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string),
        resource_url: payload
            .resource_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string),
        expiry_date,
    })
}

// ── MiniMax OAuth refresh ──────────────────────────────────────────────
//
// Restored as a per-alias schema-mirror flow: the operator's
// `oauth_refresh_token` is exchanged at MinimaxModelProvider construction
// time for a short-lived access token, which becomes the API credential.
// Region selection follows the existing `MinimaxEndpoint` enum on the
// alias config — no `MINIMAX_OAUTH_REGION` env-var needed.

#[derive(Debug, Deserialize)]
struct MinimaxOauthRefreshResponse {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    base_resp: Option<MinimaxOauthBaseResponse>,
}

#[derive(Debug, Deserialize)]
struct MinimaxOauthBaseResponse {
    #[serde(default)]
    status_msg: Option<String>,
}

/// Exchange a long-lived MiniMax `oauth_refresh_token` for a short-lived
/// access token. Synchronous (`reqwest::blocking`) by design — this runs
/// during provider construction, before any async runtime is necessarily
/// available; matches the pre-deletion behavior.
pub(crate) fn refresh_minimax_oauth_access_token(
    refresh_token: &str,
    client_id: &str,
    region: zeroclaw_config::schema::MinimaxEndpoint,
) -> anyhow::Result<String> {
    let endpoint = region.oauth_token_endpoint();
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new());

    let response = client
        .post(endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ])
        .send()
        .map_err(|error| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "oauth_provider": "minimax",
                        "phase": "refresh_request",
                        "error": format!("{}", error),
                    })),
                "minimax: OAuth refresh request failed"
            );
            anyhow::Error::msg(format!("MiniMax OAuth refresh request failed: {error}"))
        })?;

    let status = response.status();
    let body = response
        .text()
        .unwrap_or_else(|_| "<failed to read MiniMax OAuth response body>".to_string());
    let parsed = serde_json::from_str::<MinimaxOauthRefreshResponse>(&body).ok();

    if !status.is_success() {
        let detail = parsed
            .as_ref()
            .and_then(|payload| payload.base_resp.as_ref())
            .and_then(|base| base.status_msg.as_deref())
            .filter(|msg| !msg.trim().is_empty())
            .unwrap_or(body.as_str());
        anyhow::bail!("MiniMax OAuth refresh failed (HTTP {status}): {detail}");
    }

    if let Some(payload) = parsed {
        if let Some(status_text) = payload.status.as_deref()
            && !status_text.eq_ignore_ascii_case("success")
        {
            let detail = payload
                .base_resp
                .as_ref()
                .and_then(|base| base.status_msg.as_deref())
                .unwrap_or(status_text);
            anyhow::bail!("MiniMax OAuth refresh failed: {detail}");
        }
        if let Some(token) = payload
            .access_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
        {
            return Ok(token.to_string());
        }
    }
    anyhow::bail!("MiniMax OAuth refresh response missing access_token")
}

fn resolve_qwen_oauth_context(credential_override: Option<&str>) -> QwenOauthProviderContext {
    let override_value = credential_override
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let placeholder_requested = override_value
        .map(|value| value.eq_ignore_ascii_case(QWEN_OAUTH_PLACEHOLDER))
        .unwrap_or(false);

    if let Some(explicit) = override_value
        && !placeholder_requested
    {
        return QwenOauthProviderContext {
            credential: Some(explicit.to_string()),
            base_url: None,
        };
    }

    // Qwen OAuth: file cache at `~/.qwen/oauth_creds.json` (populated by the
    // upstream Qwen CLI's `qwen login` flow) is the ambient source. Direct
    // injection goes through the schema-mirror grammar.
    let mut cached = read_qwen_oauth_cached_credentials();

    let should_refresh = cached.as_ref().is_some_and(qwen_oauth_token_expired)
        || cached
            .as_ref()
            .and_then(|credentials| credentials.access_token.as_deref())
            .is_none_or(|value| value.trim().is_empty());

    if should_refresh
        && let Some(refresh_token) = cached
            .as_ref()
            .and_then(|credentials| credentials.refresh_token.clone())
    {
        match refresh_qwen_oauth_access_token(&refresh_token, &qwen_oauth_client_id()) {
            Ok(refreshed) => {
                cached = Some(refreshed);
            }
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", error)})),
                    "OAuth refresh failed"
                );
            }
        }
    }

    let credential = cached
        .as_ref()
        .and_then(|credentials| credentials.access_token.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);

    let base_url = cached
        .as_ref()
        .and_then(|credentials| credentials.resource_url.as_deref())
        .and_then(normalize_qwen_oauth_base_url);

    QwenOauthProviderContext {
        credential,
        base_url,
    }
}

// `canonical_china_provider_name` and the per-family `*_base_url(name)`
// lookup helpers were deleted in #6273: post-Phase-8 migration the runtime
// only sees canonical family names (`"moonshot"`, `"qwen"`, `"glm"`,
// `"minimax"`, `"zai"`, `"doubao"`, `"qianfan"`), and per-instance URLs
// flow through `ModelProviderRuntimeOptions.provider_api_url` (pre-resolved
// from the typed alias's `*Endpoint::uri()` by
// `provider_runtime_options_for_agent`). Synonym detection lives only in
// `crates/zeroclaw-config/src/schema/v2.rs::normalize_model_provider_type`.

#[derive(Debug, Clone)]
pub struct ModelProviderRuntimeOptions {
    pub auth_profile_override: Option<String>,
    pub provider_api_url: Option<String>,
    pub zeroclaw_dir: Option<PathBuf>,
    pub secrets_encrypt: bool,
    pub reasoning_enabled: Option<bool>,
    pub reasoning_effort: Option<String>,
    /// HTTP request timeout in seconds for LLM model_provider API calls.
    /// `None` uses the model_provider's built-in default (120s for compatible model_providers).
    pub provider_timeout_secs: Option<u64>,
    /// Extra HTTP headers to include in model_provider API requests.
    pub extra_headers: std::collections::HashMap<String, String>,
    /// Custom API path suffix for OpenAI-compatible model_providers
    /// (e.g. "/v2/generate" instead of the default "/chat/completions").
    pub api_path: Option<String>,
    /// Maximum output tokens for LLM model_provider API requests.
    /// `None` uses the model_provider's built-in default.
    pub provider_max_tokens: Option<u32>,
    /// When true, system messages are merged into the first user message before
    /// sending. Propagated from `ModelProviderConfig::merge_system_into_user`.
    pub merge_system_into_user: bool,
    /// Extra JSON parameters merged into API request bodies at the top level.
    /// Propagated from `ModelProviderConfig::provider_extra`.
    pub provider_extra: Option<serde_json::Value>,
    /// When set, the provider is asked to use its native tool-calling
    /// schema instead of OpenAI-compat tool calls. Generic across families.
    pub native_tools: Option<bool>,
    /// Wire protocol to use for this provider.
    /// `Some("responses")` routes the provider through the OpenResponses
    /// `/v1/responses` API instead of chat_completions.  `None` uses the
    /// provider's built-in default (chat_completions for most providers).
    pub wire_api: Option<String>,
    /// Enable or disable chain-of-thought thinking. Forwarded as
    /// `enable_thinking` in the request body. `None` lets the model decide.
    pub think: Option<bool>,
    /// Passed verbatim as `chat_template_kwargs` to the llamacpp provider.
    pub chat_template_kwargs: Option<serde_json::Value>,
}

impl Default for ModelProviderRuntimeOptions {
    fn default() -> Self {
        Self {
            auth_profile_override: None,
            provider_api_url: None,
            zeroclaw_dir: None,
            secrets_encrypt: true,
            reasoning_enabled: None,
            reasoning_effort: None,
            provider_timeout_secs: None,
            extra_headers: std::collections::HashMap::new(),
            api_path: None,
            provider_max_tokens: None,
            merge_system_into_user: false,
            provider_extra: None,
            native_tools: None,
            wire_api: None,
            think: None,
            chat_template_kwargs: None,
        }
    }
}

/// Build `ModelProviderRuntimeOptions` from a *specific* `ModelProviderConfig`
/// entry plus the global config's process-wide settings (zeroclaw_dir,
/// secrets, runtime). Splits out the per-entry resolution so callers with
/// agent context can pass in the alias-resolved entry instead of hitting
/// `first_model_provider()`.
///
/// Pass `None` when no model_provider entry is resolvable (e.g. tests or fresh
/// config with no models configured); falls back to safe defaults.
pub fn model_provider_runtime_options_from_model_provider_entry(
    config: &zeroclaw_config::schema::Config,
    entry: Option<&zeroclaw_config::schema::ModelProviderConfig>,
) -> ModelProviderRuntimeOptions {
    // Resolve merge_system_into_user from the active model model_provider profile
    // by matching api_url — providers.models retains all profiles. We keep
    // this lookup based on URL match rather than identity because the entry
    // we were given may itself originate from any of those profiles.
    let merge_system_into_user = entry
        .and_then(|e| e.uri.as_deref())
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .and_then(|active_uri| {
            config
                .providers
                .models
                .iter_entries()
                .map(|(_, _, base)| base)
                .find(|p| {
                    p.uri
                        .as_deref()
                        .map(str::trim)
                        .filter(|u: &&str| !u.is_empty())
                        .map(|u: &str| u.trim_end_matches('/'))
                        == Some(active_uri.trim_end_matches('/'))
                })
        })
        .map(|p| p.merge_system_into_user)
        .unwrap_or(false);

    ModelProviderRuntimeOptions {
        auth_profile_override: None,
        provider_api_url: entry.and_then(|e| e.uri.clone()),
        zeroclaw_dir: config.config_path.parent().map(PathBuf::from),
        secrets_encrypt: config.secrets.encrypt,
        reasoning_enabled: config.runtime.reasoning_enabled,
        reasoning_effort: config.runtime.reasoning_effort.clone(),
        provider_timeout_secs: Some(entry.and_then(|e| e.timeout_secs).unwrap_or(120)),
        extra_headers: entry.map(|e| e.extra_headers.clone()).unwrap_or_default(),
        api_path: None,
        provider_max_tokens: entry.and_then(|e| e.max_tokens),
        merge_system_into_user,
        provider_extra: entry.and_then(|e| e.provider_extra.clone()),
        native_tools: entry.and_then(|e| e.native_tools),
        wire_api: entry.and_then(|e| e.wire_api.map(|w| w.as_str().to_string())),
        think: entry.and_then(|e| e.think),
        chat_template_kwargs: entry.and_then(|e| e.chat_template_kwargs.clone()),
    }
}

/// Resolve `ModelProviderRuntimeOptions` from an agent's `model_provider` alias
/// (`"<type>.<alias>"`). Falls back to `first_model_provider()` when the agent
/// alias doesn't exist, doesn't have a `model_provider` set, or names a
/// non-existent model_provider entry.
pub fn provider_runtime_options_for_agent(
    config: &zeroclaw_config::schema::Config,
    agent_alias: &str,
) -> ModelProviderRuntimeOptions {
    let entry = config.model_provider_for_agent(agent_alias).or_else(|| {
        ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"agent": agent_alias})), "model_provider_for_agent returned None; falling back to model_providers.first_model_provider()");
        config.first_model_provider()
    });
    let mut options = model_provider_runtime_options_from_model_provider_entry(config, entry);

    if let Some(agent) = config.agents.get(agent_alias)
        && let Some((family, alias)) = agent.model_provider.split_once('.')
    {
        // Multi-endpoint families: pre-resolve the URI via the centralized
        // `resolved_endpoint_uri` dispatch (driven by
        // `for_each_model_provider_slot!`). Operator-set `base.uri` already
        // populated above wins over the family default.
        if options.provider_api_url.is_none()
            && let Some(uri) = config.providers.models.resolved_endpoint_uri(family, alias)
        {
            options.provider_api_url = Some(uri.to_string());
        }
        // Family-specific typed extras (Azure resource, kilocli/gemini_cli
        // binary_path, Gemini OAuth client credentials, OpenAI Codex
        // auth-routing, etc.) are read directly by the factory branches
        // from `config.providers.models.<family>.<alias>` — no flat
        // dumping ground here.
    }

    options
}

pub fn provider_runtime_options_from_config(
    config: &zeroclaw_config::schema::Config,
) -> ModelProviderRuntimeOptions {
    model_provider_runtime_options_from_model_provider_entry(config, config.first_model_provider())
}

/// Build runtime options for a specific dotted provider alias
/// (`<family>.<alias>`). Mirrors `provider_runtime_options_for_agent` but
/// keyed on the typed provider entry directly, so routed providers can
/// resolve their alias-specific endpoint URI and other typed extras
/// without going through an owning agent.
pub fn provider_runtime_options_for_alias(
    config: &zeroclaw_config::schema::Config,
    family: &str,
    alias: &str,
) -> ModelProviderRuntimeOptions {
    let entry = config.providers.models.find(family, alias);
    let mut options = model_provider_runtime_options_from_model_provider_entry(config, entry);
    if options.provider_api_url.is_none()
        && let Some(uri) = config.providers.models.resolved_endpoint_uri(family, alias)
    {
        options.provider_api_url = Some(uri.to_string());
    }
    options
}

/// Options to use when building a provider from a name that may be either
/// a bare family or a dotted alias. Dotted names yield alias-resolved
/// options; bare names return `fallback` unchanged.
pub fn options_for_provider_ref(
    config: &zeroclaw_config::schema::Config,
    name: &str,
    fallback: &ModelProviderRuntimeOptions,
) -> ModelProviderRuntimeOptions {
    match name.split_once('.') {
        Some((family, alias)) => provider_runtime_options_for_alias(config, family, alias),
        None => fallback.clone(),
    }
}

fn is_secret_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':')
}

fn token_end(input: &str, from: usize) -> usize {
    let mut end = from;
    for (i, c) in input[from..].char_indices() {
        if is_secret_char(c) {
            end = from + i + c.len_utf8();
        } else {
            break;
        }
    }
    end
}

/// Scrub known secret-like token prefixes from model_provider error strings.
///
/// Redacts tokens with prefixes like `sk-`, `xoxb-`, `xoxp-`, `ghp_`, `gho_`,
/// `ghu_`, and `github_pat_`.
pub fn scrub_secret_patterns(input: &str) -> String {
    const PREFIXES: [&str; 7] = [
        "sk-",
        "xoxb-",
        "xoxp-",
        "ghp_",
        "gho_",
        "ghu_",
        "github_pat_",
    ];

    let mut scrubbed = input.to_string();

    for prefix in PREFIXES {
        let mut search_from = 0;
        while let Some(rel) = scrubbed[search_from..].find(prefix) {
            let start = search_from + rel;
            let content_start = start + prefix.len();
            let end = token_end(&scrubbed, content_start);

            // Bare prefixes like "sk-" should not stop future scans.
            if end == content_start {
                search_from = content_start;
                continue;
            }

            scrubbed.replace_range(start..end, "[REDACTED]");
            search_from = start + "[REDACTED]".len();
        }
    }

    scrubbed
}

/// Sanitize API error text by scrubbing secrets and truncating length.
pub fn sanitize_api_error(input: &str) -> String {
    let scrubbed = scrub_secret_patterns(input);

    if scrubbed.chars().count() <= MAX_API_ERROR_CHARS {
        return scrubbed;
    }

    let mut end = MAX_API_ERROR_CHARS;
    while end > 0 && !scrubbed.is_char_boundary(end) {
        end -= 1;
    }

    format!("{}...", &scrubbed[..end])
}

/// Format an error including its full source chain and sanitize the result.
pub fn format_error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut formatted = String::new();
    let _ = std::fmt::Write::write_fmt(&mut formatted, format_args!("{error}"));
    let mut current = error.source();
    while let Some(source) = current {
        let _ = std::fmt::Write::write_fmt(&mut formatted, format_args!(": {source}"));
        current = source.source();
    }
    sanitize_api_error(&formatted)
}

/// Build a sanitized model_provider error from a failed HTTP response.
pub async fn api_error(model_provider: &str, response: reqwest::Response) -> anyhow::Error {
    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "<failed to read model_provider error body>".to_string());
    let sanitized = sanitize_api_error(&body);
    ::zeroclaw_log::record!(
        ERROR,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "model_provider": model_provider,
                "status": status.as_u16(),
                "body": sanitized,
            })),
        "providers: API error"
    );
    anyhow::Error::msg(format!(
        "{model_provider} API error ({status}): {sanitized}"
    ))
}

/// Resolve API key for a model_provider from config and environment variables.
///
/// Return the typed-alias `api_key` field, trimmed. Env-var overrides land on
/// the field at config-load via the `ZEROCLAW_*` schema-mirror grammar.
fn resolve_model_provider_credential(
    _name: &str,
    credential_override: Option<&str>,
) -> Option<String> {
    credential_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

/// Single source of truth for `(key_prefix, canonical_model_provider_family)`
/// pairs used by `check_api_key_prefix`. Order matters: longer prefixes
/// must come before shorter ones that share a head (`sk-ant-` and `sk-or-`
/// must precede `sk-`).
const KEY_PREFIX_MODEL_PROVIDERS: &[(&str, &str)] = &[
    ("sk-ant-", "anthropic"),
    ("sk-or-", "openrouter"),
    ("sk-", "openai"),
    ("gsk_", "groq"),
    ("pplx-", "perplexity"),
    ("xai-", "xai"),
    ("nvapi-", "nvidia"),
    ("KEY-", "telnyx"),
];

/// Check whether an API key's prefix matches the selected model model_provider.
///
/// Returns `Some("likely_model_provider")` when the key clearly belongs to a
/// *different* model model_provider (cross-provider mismatch). Returns `None`
/// when everything looks fine or the format is unrecognised.
fn check_api_key_prefix(model_provider_name: &str, key: &str) -> Option<&'static str> {
    let likely_model_provider = KEY_PREFIX_MODEL_PROVIDERS
        .iter()
        .find(|(prefix, _)| key.starts_with(prefix))
        .map(|(_, name)| *name)?;

    // Only flag mismatch when the configured `model_provider_name` is itself
    // one whose key format we recognize — derived from the same table so the
    // gate can never drift from the prefix detection above.
    let recognized = KEY_PREFIX_MODEL_PROVIDERS
        .iter()
        .any(|(_, name)| *name == model_provider_name);
    if !recognized {
        return None;
    }

    if model_provider_name == likely_model_provider {
        None
    } else {
        Some(likely_model_provider)
    }
}

// `parse_custom_provider_url` was deleted in #6273. The legacy colon-URL form
// (`custom:https://...` and `anthropic-custom:https://...`) is collapsed
// at TOML load time by `normalize_model_provider_type` in `schema/v2.rs` into
// `[model_providers.custom.<alias>] uri = "..."` (or
// `[model_providers.anthropic.custom] uri = "..."`). The factory's
// `"custom"` arm reads `uri` from the alias entry via
// `options.provider_api_url`; URL parsing/validation now happens at
// schema validation time, not at runtime construction.

/// Factory: create the right model_provider from config (without custom URL).
///
/// Legacy entry point — no per-alias typed extras visible. Calls the
/// `_for_alias` variant with default per-family config; suitable for
/// tests and programmatic callers using compat families that don't read
/// from the typed alias config struct. Production callers with agent
/// context should use [`create_model_provider_for_alias`].
pub fn create_model_provider(
    name: &str,
    api_key: Option<&str>,
) -> anyhow::Result<Box<dyn ModelProvider>> {
    create_model_provider_inner(
        None,
        name,
        "default",
        api_key,
        None,
        &ModelProviderRuntimeOptions::default(),
    )
}

/// Factory: create model_provider with runtime options.
///
/// Legacy entry point — see [`create_model_provider`].
pub fn create_model_provider_with_options(
    name: &str,
    api_key: Option<&str>,
    options: &ModelProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn ModelProvider>> {
    create_model_provider_inner(None, name, "default", api_key, None, options)
}

/// Factory: create model_provider with optional custom base URL.
///
/// Legacy entry point — see [`create_model_provider`].
pub fn create_model_provider_with_url(
    name: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
) -> anyhow::Result<Box<dyn ModelProvider>> {
    create_model_provider_inner(
        None,
        name,
        "default",
        api_key,
        api_url,
        &ModelProviderRuntimeOptions::default(),
    )
}

/// Factory: create model_provider with full alias context.
///
/// `(config, family, alias)` lets each family branch read its own typed
/// alias config (`config.providers.models.<family>.get(alias)`) directly
/// — no flat per-family extras dumping ground. Production callers with
/// agent context (delegate, llm_task, model routing, gateway) use this.
pub fn create_model_provider_for_alias(
    config: &zeroclaw_config::schema::Config,
    family: &str,
    alias: &str,
    api_key: Option<&str>,
    options: &ModelProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn ModelProvider>> {
    create_model_provider_inner(Some(config), family, alias, api_key, None, options)
}

/// Factory: create model_provider with alias context AND custom base URL.
pub fn create_model_provider_for_alias_with_url(
    config: &zeroclaw_config::schema::Config,
    family: &str,
    alias: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
    options: &ModelProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn ModelProvider>> {
    create_model_provider_inner(Some(config), family, alias, api_key, api_url, options)
}

/// Map a V2 model-provider family name (synonyms, regional variants, OAuth
/// suffixes) to its V3 canonical family. Production configs are normalised at
/// TOML load time by `normalize_provider_type` in
/// `zeroclaw-config/src/schema/v2.rs`. This helper duplicates the same table
/// at the runtime factory boundary so callers that bypass the schema
/// migration (programmatic factory invocations, tests, the
/// `create_model_provider_with_url` colon-URL legacy entry point) still
/// resolve. Inputs that are already canonical or unknown pass through
/// unchanged.
#[must_use]
pub fn canonicalize_v2_model_provider_name(name: &str) -> &str {
    match name {
        // Vendor-canonical synonyms.
        "azure_openai" | "azure-openai" => "azure",
        "grok" => "xai",
        "google" | "google-gemini" => "gemini",
        "together-ai" => "together",
        "fireworks-ai" => "fireworks",
        "vercel-ai" => "vercel",
        "cloudflare-ai" => "cloudflare",
        "nvidia-nim" | "build.nvidia.com" => "nvidia",
        "aws-bedrock" => "bedrock",
        "lm-studio" => "lmstudio",
        "lite-llm" => "litellm",
        "hf" => "huggingface",
        "01ai" | "lingyiwanwu" => "yi",
        "tencent" => "hunyuan",
        "baidu" => "qianfan",
        "github-copilot" => "copilot",
        "ovhcloud" => "ovh",
        "opencode-zen" => "opencode",
        "llama.cpp" => "llamacpp",
        "deep-myst" => "deepmyst",
        "silicon-flow" => "siliconflow",
        "deep-infra" => "deepinfra",
        "ai21-labs" => "ai21",
        "friendliai" => "friendli",
        "lepton-ai" => "lepton",
        "step" => "stepfun",
        "kilo" => "kilocli",
        // Moonshot / Kimi (regional + code variants fold to one family).
        "kimi" | "kimi-cn" | "kimi-intl" | "kimi-global" | "kimi-code" | "kimi_coding"
        | "kimi_for_coding" | "moonshot-cn" | "moonshot-intl" | "moonshot-global" => "moonshot",
        // Qwen / DashScope / Bailian.
        "qwen-cn"
        | "qwen-intl"
        | "qwen-us"
        | "qwen-international"
        | "qwen-code"
        | "qwen-oauth"
        | "qwen_oauth"
        | "dashscope"
        | "dashscope-cn"
        | "dashscope-intl"
        | "dashscope-us"
        | "dashscope-international"
        | "bailian"
        | "aliyun-bailian"
        | "aliyun" => "qwen",
        // GLM / Zhipu.
        "zhipu" | "glm-global" | "zhipu-global" | "glm-cn" | "zhipu-cn" | "bigmodel" => "glm",
        // Z.AI.
        "z.ai" | "zai-global" | "z.ai-global" | "zai-cn" | "z.ai-cn" => "zai",
        // Minimax (cn/intl + oauth).
        "minimax-intl"
        | "minimax-io"
        | "minimax-global"
        | "minimax-portal"
        | "minimax-portal-global"
        | "minimax-cn"
        | "minimaxi"
        | "minimax-portal-cn"
        | "minimax-oauth"
        | "minimax-oauth-global"
        | "minimax-oauth-cn" => "minimax",
        // Doubao / Volcengine.
        "volcengine" | "ark" | "doubao-cn" => "doubao",
        // Gemini CLI is its own typed slot (subprocess runtime).
        "gemini-cli" => "gemini_cli",
        // Stepfun-intl folds with a different uri at the schema layer.
        "stepfun-intl" | "step-intl" => "stepfun",
        // Anthropic special folds.
        "claude-code" | "anthropic-custom" => "anthropic",
        // OpenCode regional fold (alias differs at the schema layer).
        "opencode-go" => "opencode",
        // Already canonical, or a name the factory's match arms can reject
        // with a useful error.
        _ => name,
    }
}

/// Split a V2 colon-URL family name (`custom:https://...`,
/// `anthropic-custom:https://...`) into a `(name, url)` pair. The V3 typed
/// schema stores custom endpoints as `[model_providers.<family>.<alias>]
/// uri = "..."`; this helper preserves runtime-factory compatibility for
/// callers that still pass the legacy single-token form.
fn split_v2_colon_url(name: &str) -> (&str, Option<&str>) {
    if let Some(idx) = name.find(':') {
        let (prefix, rest) = name.split_at(idx);
        let url = &rest[1..];
        if url.starts_with("http://") || url.starts_with("https://") {
            return (prefix, Some(url));
        }
    }
    (name, None)
}

pub(crate) fn moonshot_code_base_url() -> &'static str {
    <zeroclaw_config::schema::MoonshotEndpoint as zeroclaw_config::schema::ModelEndpoint>::uri(
        &zeroclaw_config::schema::MoonshotEndpoint::Code,
    )
}

fn is_legacy_kimi_code_alias(name: &str) -> bool {
    matches!(name, "kimi-code" | "kimi_coding" | "kimi_for_coding")
}

/// Factory: create model_provider with optional base URL and runtime options.
#[allow(clippy::too_many_lines)]
fn create_model_provider_inner(
    config: Option<&zeroclaw_config::schema::Config>,
    raw_name: &str,
    alias: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
    options: &ModelProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn ModelProvider>> {
    // Pre-normalise the family name for callers that bypass the schema
    // migration (tests, programmatic factory calls, V2 colon-URL form).
    // Detect the bare `custom:` and `anthropic-custom:` forms (colon present,
    // URL missing or malformed) and surface a useful error before falling
    // into the unknown-family arm.
    if let Some(idx) = raw_name.find(':') {
        let prefix = &raw_name[..idx];
        let url = raw_name[idx + 1..].trim();
        if matches!(prefix, "custom" | "anthropic-custom")
            && (url.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")))
        {
            anyhow::bail!(
                "Custom model_provider `{prefix}:<url>` requires a URL beginning with http:// or https://. \
                 Set `[model_providers.custom.<alias>] uri = \"https://your-api.com\"` or pass a valid URL."
            );
        }
    }
    let (split_name, split_url) = split_v2_colon_url(raw_name);
    let legacy_kimi_code = is_legacy_kimi_code_alias(split_name);
    let api_url = api_url.or(split_url);
    let name = canonicalize_v2_model_provider_name(split_name);

    // V2 spelled OpenAI Codex as `openai-codex` / `openai_codex` / `codex`.
    // V3 dispatches via `requires_openai_auth = true` on the typed alias, but
    // factory callers that pass the legacy spelling expect a working
    // construction here.
    if matches!(name, "openai-codex" | "openai_codex" | "codex") {
        return Ok(Box::new(openai_codex::OpenAiCodexModelProvider::new(
            alias, options, api_key,
        )?));
    }
    // Resolve credential and break static-analysis taint chain from the
    // `api_key` parameter so that downstream model_provider storage of the value
    // is not linked to the original sensitive-named source. Qwen OAuth
    // alias detection moved into `QwenModelProviderConfig::create_provider`
    // — the per-family impl owns its own credential-resolution logic.
    let resolved_credential = resolve_model_provider_credential(name, api_key)
        .map(|v| String::from_utf8(v.into_bytes()).unwrap_or_default());
    #[allow(clippy::option_as_ref_deref)]
    let key = resolved_credential.as_ref().map(String::as_str);

    // Pre-flight: catch obvious API-key / model_provider mismatches early.
    if let Some(key_value) = key {
        let is_custom = name.starts_with("custom:") || name.starts_with("anthropic-custom:");
        let has_custom_url = api_url.map(str::trim).filter(|u| !u.is_empty()).is_some();
        if !is_custom
            && !has_custom_url
            && let Some(likely_model_provider) = check_api_key_prefix(name, key_value)
        {
            let visible = &key_value[..key_value.len().min(8)];
            anyhow::bail!(
                "API key prefix mismatch: key \"{visible}...\" looks like a \
                     {likely_model_provider} key, but model_provider \"{name}\" is selected. \
                     Set the correct provider-specific env var or use `-p {likely_model_provider}`."
            );
        }
    }

    // The factory dispatches by canonical model model_provider family name only —
    // legacy synonyms ("openai-codex", "azure-openai", "google", etc.) are
    // collapsed at TOML load time by `normalize_model_provider_type` in
    // `crates/zeroclaw-config/src/schema/v2.rs`. Multi-endpoint families
    // (moonshot/qwen/glm/minimax/zai) get their URI pre-resolved into
    // `options.provider_api_url` from the typed alias's `endpoint` field
    // by `provider_runtime_options_for_agent`. Local-only families
    // (lmstudio/llamacpp/sglang/vllm/osaurus) accept either an explicit
    // `api_url` operator override or fall back to the family's localhost
    // default. Codex variant routing is handled by `create_model_provider_with_options`
    // via `options.requires_openai_auth` before this function is called.

    // Resolve the effective endpoint URL for the dispatch arms below.
    // Precedence: `api_url` parameter (operator-set base.uri), then
    // `options.provider_api_url` (pre-resolved family endpoint URI from the
    // typed alias's `*Endpoint::uri()` for multi-endpoint families).
    let resolved_url: Option<&str> =
        api_url
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .or_else(|| {
                options
                    .provider_api_url
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
            });

    if legacy_kimi_code {
        let base_url = match resolved_url {
            Some(url) => url,
            None => moonshot_code_base_url(),
        };
        return Ok(factory::apply_compat_options(
            factory::build_kimi_code_compat(alias, key, base_url),
            options,
        ));
    }

    factory::dispatch_family_factory(config, name, alias, key, resolved_url, options)
}

/// Wrap the primary model_provider in a retry/backoff harness, threading auth runtime options.
///
/// Legacy entry point — no per-alias typed extras. Codex routing now
/// happens inside `OpenAIModelProviderConfig::create_provider` driven by
/// the alias's `base.requires_openai_auth` flag. Production callers that
/// have agent context should use [`create_resilient_model_provider_for_alias`]
/// to surface family-specific config like Azure resource/deployment.
pub fn create_resilient_model_provider_with_options(
    primary_name: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
    reliability: &zeroclaw_config::schema::ReliabilityConfig,
    options: &ModelProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn ModelProvider>> {
    let primary_model_provider =
        create_model_provider_inner(None, primary_name, "default", api_key, api_url, options)?;

    let reliable = ReliableModelProvider::new(
        primary_name,
        vec![(primary_name.to_string(), primary_model_provider)],
        reliability.provider_retries,
        reliability.provider_backoff_ms,
    )
    .with_api_keys(reliability.api_keys.clone());

    Ok(Box::new(reliable))
}

/// Wrap the primary model_provider in a retry/backoff harness with full
/// alias context. Production callers (gateway, orchestrator) use this so
/// the dispatch sees the typed alias config and routes Azure/Codex/Gemini
/// extras correctly.
pub fn create_resilient_model_provider_for_alias(
    config: &zeroclaw_config::schema::Config,
    family: &str,
    alias: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
    reliability: &zeroclaw_config::schema::ReliabilityConfig,
    options: &ModelProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn ModelProvider>> {
    let primary_model_provider =
        create_model_provider_inner(Some(config), family, alias, api_key, api_url, options)?;

    let reliable = ReliableModelProvider::new(
        alias,
        vec![(family.to_string(), primary_model_provider)],
        reliability.provider_retries,
        reliability.provider_backoff_ms,
    )
    .with_api_keys(reliability.api_keys.clone());

    Ok(Box::new(reliable))
}

/// Build a resilient model provider from a name that may be either a bare
/// family (`"openai"`) or a dotted alias (`"openai.work"`). Dotted names
/// dispatch through the typed alias factory so endpoint URI, family
/// extras, and per-alias credentials from `[model_providers.<family>.<alias>]`
/// are honored; bare names route through the family factory directly.
pub fn create_resilient_model_provider_from_ref(
    config: &zeroclaw_config::schema::Config,
    name: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
    reliability: &zeroclaw_config::schema::ReliabilityConfig,
    options: &ModelProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn ModelProvider>> {
    match name.split_once('.') {
        Some((family, alias)) => create_resilient_model_provider_for_alias(
            config,
            family,
            alias,
            api_key,
            api_url,
            reliability,
            options,
        ),
        None => create_resilient_model_provider_with_options(
            name,
            api_key,
            api_url,
            reliability,
            options,
        ),
    }
}

/// Build a router fronted by `primary_name` plus one provider per unique
/// `model_routes` entry. Each dotted `<family>.<alias>` name resolves
/// through the typed `[model_providers.<family>.<alias>]` config (endpoint
/// URI, Azure resource, Gemini OAuth, etc.); bare family names use family
/// defaults.
pub fn create_routed_model_provider_with_options(
    config: &zeroclaw_config::schema::Config,
    primary_name: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
    reliability: &zeroclaw_config::schema::ReliabilityConfig,
    model_routes: &[zeroclaw_config::schema::ModelRouteConfig],
    default_model: &str,
    options: &ModelProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn ModelProvider>> {
    if model_routes.is_empty() {
        return create_resilient_model_provider_from_ref(
            config,
            primary_name,
            api_key,
            api_url,
            reliability,
            options,
        );
    }

    // Collect unique model_provider names needed
    let mut needed: Vec<String> = vec![primary_name.to_string()];
    for route in model_routes {
        if !needed.iter().any(|n| n == &route.model_provider) {
            needed.push(route.model_provider.clone());
        }
    }

    // Create each model_provider (with its own resilience wrapper). Each
    // entry's options come from its own typed alias block when dotted;
    // the primary inherits the caller's options (already alias-resolved
    // upstream for the owning agent).
    let mut model_providers: Vec<(String, Box<dyn ModelProvider>)> = Vec::new();
    for name in &needed {
        let routed_credential = model_routes
            .iter()
            .find(|r| &r.model_provider == name)
            .and_then(|r| {
                r.api_key.as_ref().and_then(|raw_key| {
                    let trimmed_key = raw_key.trim();
                    (!trimmed_key.is_empty()).then_some(trimmed_key)
                })
            });
        let key = routed_credential
            .or_else(|| {
                name.split_once('.')
                    .and_then(|(family, alias)| {
                        config
                            .providers
                            .models
                            .find(family, alias)
                            .and_then(|cfg| cfg.api_key.as_deref())
                    })
                    .and_then(|raw_key| {
                        let trimmed = raw_key.trim();
                        (!trimmed.is_empty()).then_some(trimmed)
                    })
            })
            .or(api_key);
        let url = if name == primary_name { api_url } else { None };
        let entry_options = if name == primary_name {
            options.clone()
        } else {
            options_for_provider_ref(config, name, options)
        };

        match create_resilient_model_provider_from_ref(
            config,
            name,
            key,
            url,
            reliability,
            &entry_options,
        ) {
            Ok(model_provider) => model_providers.push((name.clone(), model_provider)),
            Err(e) => {
                if name == primary_name {
                    return Err(e);
                }
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"model_provider": name.as_str(), "error": format!("{}", e)})
                        ),
                    "Ignoring routed model_provider that failed to initialize"
                );
            }
        }
    }

    // Build route table
    let routes: Vec<(String, router::Route)> = model_routes
        .iter()
        .map(|r| {
            (
                r.hint.clone(),
                router::Route {
                    provider_name: r.model_provider.clone(),
                    model: r.model.clone(),
                },
            )
        })
        .collect();

    Ok(Box::new(router::RouterModelProvider::new(
        primary_name,
        model_providers,
        routes,
        default_model.to_string(),
    )))
}

/// Information about a supported model model_provider for display purposes.
pub struct ModelProviderInfo {
    /// Canonical name used in config (e.g. `"openrouter"`)
    pub name: &'static str,
    /// Human-readable display name
    pub display_name: &'static str,
    /// Whether the model model_provider runs locally (no API key required)
    pub local: bool,
}

/// Canonical base URL for `name`, mirroring what `create_model_provider`
/// would dial. `None` for families without a fixed default (Azure, custom,
/// multi-region, CLI shims).
#[must_use]
pub fn default_model_provider_url(name: &str) -> Option<&'static str> {
    use factory::CompatFamilySpec;
    use zeroclaw_config::schema::{
        Ai21ModelProviderConfig, AihubmixModelProviderConfig, AnyscaleModelProviderConfig,
        AstraiModelProviderConfig, BaichuanModelProviderConfig, BasetenModelProviderConfig,
        CerebrasModelProviderConfig, CloudflareModelProviderConfig, CohereModelProviderConfig,
        DeepinfraModelProviderConfig, DeepseekModelProviderConfig, DoubaoModelProviderConfig,
        FireworksModelProviderConfig, FriendliModelProviderConfig, HuggingfaceModelProviderConfig,
        HyperbolicModelProviderConfig, LeptonModelProviderConfig, LitellmModelProviderConfig,
        MistralModelProviderConfig, NebiusModelProviderConfig, NovitaModelProviderConfig,
        NscaleModelProviderConfig, OpencodeModelProviderConfig, PerplexityModelProviderConfig,
        RekaModelProviderConfig, SambanovaModelProviderConfig, SglangModelProviderConfig,
        SiliconflowModelProviderConfig, SyntheticModelProviderConfig, TogetherModelProviderConfig,
        VercelModelProviderConfig, VllmModelProviderConfig, YiModelProviderConfig,
    };

    match name {
        "anthropic" => Some(anthropic::BASE_URL),
        "openai" => Some(openai::BASE_URL),
        "openrouter" => Some(openrouter::BASE_URL),
        "ollama" => Some(ollama::BASE_URL),
        "telnyx" => Some(telnyx::BASE_URL),
        "gemini" => Some(gemini::BASE_URL),
        "vercel" => Some(<VercelModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "cloudflare" => Some(<CloudflareModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "synthetic" => Some(<SyntheticModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "opencode" => Some(<OpencodeModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "doubao" => Some(<DoubaoModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "mistral" => Some(<MistralModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "deepseek" => Some(<DeepseekModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "together" => Some(<TogetherModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "fireworks" => Some(<FireworksModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "novita" => Some(<NovitaModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "perplexity" => Some(<PerplexityModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "cohere" => Some(<CohereModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "sglang" => Some(<SglangModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "vllm" => Some(<VllmModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "astrai" => Some(<AstraiModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "siliconflow" => Some(<SiliconflowModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "aihubmix" => Some(<AihubmixModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "litellm" => Some(<LitellmModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "cerebras" => Some(<CerebrasModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "sambanova" => Some(<SambanovaModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "hyperbolic" => Some(<HyperbolicModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "deepinfra" => Some(<DeepinfraModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "huggingface" => Some(<HuggingfaceModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "ai21" => Some(<Ai21ModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "reka" => Some(<RekaModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "baseten" => Some(<BasetenModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "nscale" => Some(<NscaleModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "anyscale" => Some(<AnyscaleModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "nebius" => Some(<NebiusModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "friendli" => Some(<FriendliModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "lepton" => Some(<LeptonModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "baichuan" => Some(<BaichuanModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        "yi" => Some(<YiModelProviderConfig as CompatFamilySpec>::DEFAULT_URL),
        _ => None,
    }
}

/// Return the list of all known model_providers for display in `zeroclaw model_providers list`.
///
/// This is intentionally separate from the factory match in `create_model_provider`
/// (display concern vs. construction concern).
pub fn list_model_providers() -> Vec<ModelProviderInfo> {
    vec![
        // ── Primary model_providers ────────────────────────────────
        ModelProviderInfo {
            name: "openrouter",
            display_name: "OpenRouter",
            local: false,
        },
        ModelProviderInfo {
            name: "anthropic",
            display_name: "Anthropic",
            local: false,
        },
        ModelProviderInfo {
            name: "openai",
            display_name: "OpenAI",
            local: false,
        },
        ModelProviderInfo {
            name: "telnyx",
            display_name: "Telnyx",
            local: false,
        },
        ModelProviderInfo {
            name: "azure",
            display_name: "Azure OpenAI",
            local: false,
        },
        ModelProviderInfo {
            name: "ollama",
            display_name: "Ollama",
            local: true,
        },
        ModelProviderInfo {
            name: "gemini",
            display_name: "Google Gemini",
            local: false,
        },
        // ── OpenAI-compatible model_providers ──────────────────────
        ModelProviderInfo {
            name: "venice",
            display_name: "Venice",
            local: false,
        },
        ModelProviderInfo {
            name: "vercel",
            display_name: "Vercel AI Gateway",
            local: false,
        },
        ModelProviderInfo {
            name: "cloudflare",
            display_name: "Cloudflare AI",
            local: false,
        },
        ModelProviderInfo {
            name: "moonshot",
            display_name: "Moonshot",
            local: false,
        },
        ModelProviderInfo {
            name: "synthetic",
            display_name: "Synthetic",
            local: false,
        },
        ModelProviderInfo {
            name: "opencode",
            display_name: "OpenCode",
            local: false,
        },
        ModelProviderInfo {
            name: "zai",
            display_name: "Z.AI",
            local: false,
        },
        ModelProviderInfo {
            name: "glm",
            display_name: "GLM (Zhipu)",
            local: false,
        },
        ModelProviderInfo {
            name: "minimax",
            display_name: "MiniMax",
            local: false,
        },
        ModelProviderInfo {
            name: "bedrock",
            display_name: "Amazon Bedrock",
            local: false,
        },
        ModelProviderInfo {
            name: "qianfan",
            display_name: "Qianfan (Baidu)",
            local: false,
        },
        ModelProviderInfo {
            name: "doubao",
            display_name: "Doubao (Volcengine)",
            local: false,
        },
        ModelProviderInfo {
            name: "qwen",
            display_name: "Qwen (DashScope / Qwen Code OAuth)",
            local: false,
        },
        ModelProviderInfo {
            name: "groq",
            display_name: "Groq",
            local: false,
        },
        ModelProviderInfo {
            name: "mistral",
            display_name: "Mistral",
            local: false,
        },
        ModelProviderInfo {
            name: "xai",
            display_name: "xAI (Grok)",
            local: false,
        },
        ModelProviderInfo {
            name: "deepseek",
            display_name: "DeepSeek",
            local: false,
        },
        ModelProviderInfo {
            name: "together",
            display_name: "Together AI",
            local: false,
        },
        ModelProviderInfo {
            name: "fireworks",
            display_name: "Fireworks AI",
            local: false,
        },
        ModelProviderInfo {
            name: "novita",
            display_name: "Novita AI",
            local: false,
        },
        ModelProviderInfo {
            name: "perplexity",
            display_name: "Perplexity",
            local: false,
        },
        ModelProviderInfo {
            name: "cohere",
            display_name: "Cohere",
            local: false,
        },
        ModelProviderInfo {
            name: "copilot",
            display_name: "GitHub Copilot",
            local: false,
        },
        ModelProviderInfo {
            name: "gemini_cli",
            display_name: "Gemini CLI",
            local: true,
        },
        ModelProviderInfo {
            name: "kilocli",
            display_name: "KiloCLI",
            local: true,
        },
        ModelProviderInfo {
            name: "lmstudio",
            display_name: "LM Studio",
            local: true,
        },
        ModelProviderInfo {
            name: "llamacpp",
            display_name: "llama.cpp server",
            local: true,
        },
        ModelProviderInfo {
            name: "sglang",
            display_name: "SGLang",
            local: true,
        },
        ModelProviderInfo {
            name: "vllm",
            display_name: "vLLM",
            local: true,
        },
        ModelProviderInfo {
            name: "osaurus",
            display_name: "Osaurus",
            local: true,
        },
        ModelProviderInfo {
            name: "nvidia",
            display_name: "NVIDIA NIM",
            local: false,
        },
        ModelProviderInfo {
            name: "siliconflow",
            display_name: "SiliconFlow",
            local: false,
        },
        ModelProviderInfo {
            name: "aihubmix",
            display_name: "AiHubMix",
            local: false,
        },
        ModelProviderInfo {
            name: "litellm",
            display_name: "LiteLLM",
            local: false,
        },
        ModelProviderInfo {
            name: "atomic_chat",
            display_name: "Atomic Chat",
            local: true,
        },
        // ── Fast inference ────────────────────────────────────
        ModelProviderInfo {
            name: "cerebras",
            display_name: "Cerebras",
            local: false,
        },
        ModelProviderInfo {
            name: "sambanova",
            display_name: "SambaNova",
            local: false,
        },
        ModelProviderInfo {
            name: "hyperbolic",
            display_name: "Hyperbolic",
            local: false,
        },
        // ── Model hosting platforms ──────────────────────────
        ModelProviderInfo {
            name: "deepinfra",
            display_name: "DeepInfra",
            local: false,
        },
        ModelProviderInfo {
            name: "huggingface",
            display_name: "Hugging Face",
            local: false,
        },
        ModelProviderInfo {
            name: "ai21",
            display_name: "AI21 Labs",
            local: false,
        },
        ModelProviderInfo {
            name: "reka",
            display_name: "Reka",
            local: false,
        },
        ModelProviderInfo {
            name: "baseten",
            display_name: "Baseten",
            local: false,
        },
        ModelProviderInfo {
            name: "nscale",
            display_name: "Nscale",
            local: false,
        },
        ModelProviderInfo {
            name: "anyscale",
            display_name: "Anyscale",
            local: false,
        },
        ModelProviderInfo {
            name: "nebius",
            display_name: "Nebius AI Studio",
            local: false,
        },
        ModelProviderInfo {
            name: "friendli",
            display_name: "Friendli AI",
            local: false,
        },
        ModelProviderInfo {
            name: "lepton",
            display_name: "Lepton AI",
            local: false,
        },
        // ── Chinese AI model_providers ─────────────────────────────
        ModelProviderInfo {
            name: "stepfun",
            display_name: "Stepfun",
            local: false,
        },
        ModelProviderInfo {
            name: "baichuan",
            display_name: "Baichuan",
            local: false,
        },
        ModelProviderInfo {
            name: "yi",
            display_name: "01.AI (Yi)",
            local: false,
        },
        ModelProviderInfo {
            name: "hunyuan",
            display_name: "Tencent Hunyuan",
            local: false,
        },
        // ── Cloud AI endpoints ───────────────────────────────
        ModelProviderInfo {
            name: "ovh",
            display_name: "OVHcloud AI Endpoints",
            local: false,
        },
        ModelProviderInfo {
            name: "avian",
            display_name: "Avian",
            local: false,
        },
    ]
}

/// Shared test utilities for model_provider modules.
#[cfg(test)]
pub mod test_util {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// Process-wide lock for tests that mutate environment variables.
    pub fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock poisoned")
    }

    /// RAII guard that sets or unsets an env var and restores the original
    /// value on drop. Always acquire [`env_lock`] before creating guards.
    pub struct EnvGuard {
        key: String,
        original: Option<String>,
    }

    impl EnvGuard {
        pub fn set(key: &str, value: Option<&str>) -> Self {
            let original = std::env::var(key).ok();
            match value {
                // SAFETY: test-only, single-threaded test runner.
                Some(v) => unsafe { std::env::set_var(key, v) },
                // SAFETY: test-only, single-threaded test runner.
                None => unsafe { std::env::remove_var(key) },
            }
            Self {
                key: key.to_string(),
                original,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(original) = self.original.as_deref() {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var(&self.key, original) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var(&self.key) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::{EnvGuard, env_lock};
    use super::*;

    // Compile-time proof that both reqwest TLS-root features are enabled.
    // `tls_built_in_webpki_certs` is gated on `rustls-tls-webpki-roots-no-provider`;
    // `tls_built_in_native_certs` is gated on `rustls-tls-native-roots-no-provider`.
    // If either feature were dropped, this test would fail to compile.
    #[test]
    fn provider_http_client_trusts_both_webpki_and_native_roots() {
        let _client = reqwest::Client::builder()
            .tls_built_in_webpki_certs(true)
            .tls_built_in_native_certs(true)
            .build()
            .expect("client builder should succeed with both root sets enabled");
    }

    #[test]
    fn resolve_provider_credential_returns_trimmed_override() {
        let resolved = resolve_model_provider_credential("openrouter", Some("  explicit-key  "));
        assert_eq!(resolved, Some("explicit-key".to_string()));
    }

    #[test]
    fn resolve_provider_credential_filters_empty_override() {
        assert!(resolve_model_provider_credential("openrouter", Some("   ")).is_none());
        assert!(resolve_model_provider_credential("openrouter", None).is_none());
    }

    // V0.8.0: tests that exercised env-var-driven credential resolution and
    // OAuth env-var fallbacks (`MINIMAX_*`, `QWEN_OAUTH_*`, `ANTHROPIC_API_KEY`,
    // `BEDROCK_API_KEY`, `API_KEY`, etc.) were deleted alongside the env-var
    // match in `resolve_model_provider_credential`. See the comment above
    // that fn for the schema-mirror replacement grammar.

    #[test]
    fn resolve_qwen_oauth_context_prefers_explicit_override() {
        let _env_lock = env_lock();
        let context = resolve_qwen_oauth_context(Some("  explicit-qwen-token  "));
        assert_eq!(context.credential.as_deref(), Some("explicit-qwen-token"));
        assert!(context.base_url.is_none());
    }

    #[test]
    fn resolve_qwen_oauth_context_reads_cached_credentials_file() {
        let _env_lock = env_lock();
        let fake_home = format!("/tmp/zeroclaw-qwen-oauth-home-{}-file", std::process::id());
        let creds_dir = PathBuf::from(&fake_home).join(".qwen");
        std::fs::create_dir_all(&creds_dir).unwrap();
        let creds_path = creds_dir.join("oauth_creds.json");
        std::fs::write(
            &creds_path,
            r#"{"access_token":"cached-token","refresh_token":"cached-refresh","resource_url":"https://resource.example.com","expiry_date":4102444800000}"#,
        )
        .unwrap();

        let _home_guard = EnvGuard::set("HOME", Some(fake_home.as_str()));

        let context = resolve_qwen_oauth_context(Some(QWEN_OAUTH_PLACEHOLDER));

        assert_eq!(context.credential.as_deref(), Some("cached-token"));
        assert_eq!(
            context.base_url.as_deref(),
            Some("https://resource.example.com/v1")
        );
    }

    #[test]
    fn resolve_qwen_oauth_context_returns_none_without_cache() {
        let _env_lock = env_lock();
        let fake_home = format!("/tmp/zeroclaw-qwen-oauth-home-{}-empty", std::process::id());
        let _home_guard = EnvGuard::set("HOME", Some(fake_home.as_str()));

        let context = resolve_qwen_oauth_context(Some(QWEN_OAUTH_PLACEHOLDER));
        assert!(context.credential.is_none());
    }

    #[test]
    fn regional_alias_predicates_cover_expected_variants() {
        assert!(is_moonshot_alias("moonshot"));
        assert!(is_moonshot_alias("kimi-global"));
        assert!(is_glm_alias("glm"));
        assert!(is_glm_alias("bigmodel"));
        assert!(is_minimax_alias("minimax-io"));
        assert!(is_minimax_alias("minimaxi"));
        assert!(is_minimax_alias("minimax-oauth"));
        assert!(is_minimax_alias("minimax-portal-cn"));
        assert!(is_qwen_alias("dashscope"));
        assert!(is_qwen_alias("qwen-us"));
        assert!(is_qwen_alias("qwen-code"));
        assert!(is_qwen_oauth_alias("qwen-code"));
        assert!(is_qwen_oauth_alias("qwen_oauth"));
        assert!(is_zai_alias("z.ai"));
        assert!(is_zai_alias("zai-cn"));
        assert!(is_qianfan_alias("qianfan"));
        assert!(is_qianfan_alias("baidu"));
        assert!(is_doubao_alias("doubao"));
        assert!(is_doubao_alias("volcengine"));
        assert!(is_doubao_alias("ark"));
        assert!(is_doubao_alias("doubao-cn"));

        assert!(!is_moonshot_alias("openrouter"));
        assert!(!is_glm_alias("openai"));
        assert!(!is_qwen_alias("gemini"));
        assert!(!is_zai_alias("anthropic"));
        assert!(!is_qianfan_alias("cohere"));
        assert!(!is_doubao_alias("deepseek"));
    }

    // Tests for the deleted `canonical_china_provider_name` function and
    // the `*_base_url(name)` lookup helpers were removed alongside their
    // subjects in #6273. Equivalent regional-collapse semantics are now
    // covered by the migration tests at
    // `crates/zeroclaw-config/tests/migration.rs` (`v2_model_providers_alias_wrapped`,
    // `claude_code_folded_under_anthropic`, etc.) which exercise
    // `normalize_model_provider_type` directly.

    // ── Primary model_providers ────────────────────────────────────

    #[test]
    fn factory_openrouter() {
        assert!(create_model_provider("openrouter", Some("provider-test-credential")).is_ok());
        assert!(create_model_provider("openrouter", None).is_ok());
    }

    #[test]
    fn factory_anthropic() {
        assert!(create_model_provider("anthropic", Some("provider-test-credential")).is_ok());
    }

    #[test]
    fn factory_openai() {
        assert!(create_model_provider("openai", Some("provider-test-credential")).is_ok());
    }

    #[test]
    fn factory_openai_codex() {
        // Codex is now selected by the typed `base.requires_openai_auth`
        // flag on an `[model_providers.openai.codex]` alias entry — the
        // factory's legacy escape hatch for the bare "openai-codex" /
        // "openai_codex" / "codex" family names still routes through
        // `OpenAiCodexModelProvider::new` when a real Config + alias is
        // not in scope.
        let options = ModelProviderRuntimeOptions::default();
        assert!(create_model_provider_with_options("openai-codex", None, &options).is_ok());
    }

    #[test]
    fn factory_ollama() {
        assert!(create_model_provider("ollama", None).is_ok());
        // Ollama may use API key when a remote endpoint is configured.
        assert!(create_model_provider("ollama", Some("dummy")).is_ok());
        assert!(create_model_provider("ollama", Some("any-value-here")).is_ok());
    }

    #[test]
    fn factory_gemini() {
        assert!(create_model_provider("gemini", Some("test-key")).is_ok());
        // Should also work without key (will try CLI auth)
        assert!(create_model_provider("gemini", None).is_ok());
    }

    #[test]
    fn factory_telnyx() {
        assert!(create_model_provider("telnyx", Some("test-key")).is_ok());
        assert!(create_model_provider("telnyx", None).is_ok());
    }

    // ── OpenAI-compatible model_providers ──────────────────────────

    #[test]
    fn factory_venice() {
        let model_provider = create_model_provider("venice", Some("vn-key")).unwrap();
        assert!(
            !model_provider.capabilities().native_tool_calling,
            "Venice should use prompt-guided tools, not native tool calling"
        );
    }

    #[test]
    fn factory_vercel() {
        assert!(create_model_provider("vercel", Some("key")).is_ok());
    }

    #[test]
    fn vercel_gateway_base_url_matches_public_gateway_endpoint() {
        assert_eq!(
            VERCEL_AI_GATEWAY_BASE_URL,
            "https://ai-gateway.vercel.sh/v1"
        );
    }

    #[test]
    fn factory_cloudflare() {
        assert!(create_model_provider("cloudflare", Some("key")).is_ok());
    }

    #[test]
    fn factory_moonshot() {
        assert!(create_model_provider("moonshot", Some("key")).is_ok());
    }

    #[test]
    fn factory_kimi_code_supports_vision() {
        for alias in ["kimi-code", "kimi_coding", "kimi_for_coding"] {
            let provider = create_model_provider(alias, Some("key"))
                .expect("legacy kimi-code alias should build");
            assert!(
                provider.supports_vision(),
                "alias `{alias}` should report vision capability"
            );
            assert_eq!(
                moonshot_code_base_url(),
                "https://api.moonshot.cn/coder/v1",
                "alias `{alias}` should resolve to the Moonshot code endpoint"
            );
        }
    }

    #[test]
    fn factory_kimi_code_preserves_semantics_with_url_overrides() {
        let custom_url = "https://proxy.example.test/v1";

        let provider = create_model_provider_with_url("kimi-code", Some("key"), Some(custom_url))
            .expect("legacy kimi-code alias with custom URL should build");
        assert!(provider.supports_vision());

        let provider = create_model_provider_with_options(
            "kimi-code",
            Some("key"),
            &ModelProviderRuntimeOptions {
                provider_api_url: Some(custom_url.to_string()),
                ..ModelProviderRuntimeOptions::default()
            },
        )
        .expect("legacy kimi-code alias with options URL should build");
        assert!(provider.supports_vision());
    }

    #[test]
    fn moonshot_code_endpoint_supports_vision() {
        use zeroclaw_config::schema::{Config, MoonshotEndpoint, MoonshotModelProviderConfig};

        let mut config = Config::default();
        config.providers.models.moonshot.insert(
            "code".to_string(),
            MoonshotModelProviderConfig {
                endpoint: MoonshotEndpoint::Code,
                ..MoonshotModelProviderConfig::default()
            },
        );
        let options = provider_runtime_options_for_alias(&config, "moonshot", "code");
        assert_eq!(
            options.provider_api_url.as_deref(),
            Some(moonshot_code_base_url())
        );

        let provider =
            create_model_provider_for_alias(&config, "moonshot", "code", Some("key"), &options)
                .expect("moonshot code endpoint should build");
        assert!(provider.supports_vision());
    }

    #[test]
    fn factory_synthetic() {
        assert!(create_model_provider("synthetic", Some("key")).is_ok());
    }

    #[test]
    fn factory_opencode() {
        assert!(create_model_provider("opencode", Some("key")).is_ok());
    }

    #[test]
    fn factory_opencode_go() {}

    #[test]
    fn factory_zai() {
        assert!(create_model_provider("zai", Some("key")).is_ok());
    }

    #[test]
    fn factory_glm() {
        assert!(create_model_provider("glm", Some("key")).is_ok());
    }

    #[test]
    fn factory_minimax() {
        assert!(create_model_provider("minimax", Some("key")).is_ok());
    }

    #[test]
    fn factory_minimax_supports_native_tool_calling() {
        let minimax =
            create_model_provider("minimax", Some("key")).expect("model_provider should resolve");
        assert!(minimax.supports_native_tools());
    }

    #[test]
    fn factory_bedrock() {
        // Bedrock uses AWS env vars for credentials, not API key.
        assert!(create_model_provider("bedrock", None).is_ok());
        // Passing an api_key is harmless (ignored).
        assert!(create_model_provider("bedrock", Some("ignored")).is_ok());
    }

    #[test]
    fn factory_qianfan() {
        assert!(create_model_provider("qianfan", Some("key")).is_ok());
    }

    #[test]
    fn factory_doubao() {
        assert!(create_model_provider("doubao", Some("key")).is_ok());
    }

    #[test]
    fn factory_qwen() {
        assert!(create_model_provider("qwen", Some("key")).is_ok());
    }

    #[test]
    fn qwen_provider_supports_vision() {
        let model_provider =
            create_model_provider("qwen", Some("key")).expect("qwen model_provider should build");
        assert!(model_provider.supports_vision());
    }

    #[test]
    fn glm_provider_supports_vision() {
        // GLM exposes vision-capable models (e.g. `glm-4.5v`). The provider
        // must therefore report `supports_vision()` so multimodal routing
        // can target it; the model field selects the actual variant.
        for alias in ["glm", "zhipu", "glm-cn", "zhipu-cn"] {
            let provider =
                create_model_provider(alias, Some("id.secret")).expect("glm provider should build");
            assert!(
                provider.supports_vision(),
                "alias `{alias}` should report vision capability"
            );
        }
    }

    #[test]
    fn factory_lmstudio() {
        assert!(create_model_provider("lmstudio", Some("key")).is_ok());
        assert!(create_model_provider("lmstudio", None).is_ok());
    }

    #[test]
    fn factory_llamacpp() {
        assert!(create_model_provider("llamacpp", Some("key")).is_ok());
        assert!(create_model_provider("llamacpp", None).is_ok());
    }

    #[test]
    fn factory_sglang() {
        assert!(create_model_provider("sglang", None).is_ok());
        assert!(create_model_provider("sglang", Some("key")).is_ok());
    }

    #[test]
    fn factory_vllm() {
        assert!(create_model_provider("vllm", None).is_ok());
        assert!(create_model_provider("vllm", Some("key")).is_ok());
    }

    #[test]
    fn factory_osaurus() {
        // Osaurus works without an explicit key (defaults to "osaurus").
        assert!(create_model_provider("osaurus", None).is_ok());
        // Osaurus also works with an explicit key.
        assert!(create_model_provider("osaurus", Some("custom-key")).is_ok());
    }

    #[test]
    fn factory_osaurus_uses_default_key_when_none() {
        // Verify that osaurus construction succeeds even without an API
        // key — the impl provides a default placeholder.
        let p = create_model_provider_with_url("osaurus", None, None);
        assert!(p.is_ok());
    }

    #[test]
    fn factory_osaurus_custom_url() {
        // Verify that a custom api_url overrides the default localhost endpoint.
        let p = create_model_provider_with_url(
            "osaurus",
            Some("key"),
            Some("http://192.168.1.100:1337/v1"),
        );
        assert!(p.is_ok());
    }

    #[test]
    fn resolve_provider_credential_osaurus_env_deleted() {}

    #[test]
    fn resolve_provider_credential_doubao_volcengine_env_deleted() {}

    #[test]
    fn resolve_provider_credential_aihubmix_env_deleted() {}

    #[test]
    fn resolve_provider_credential_siliconflow_env_deleted() {}

    #[test]
    fn factory_aihubmix() {
        assert!(create_model_provider("aihubmix", Some("key")).is_ok());
    }

    #[test]
    fn factory_siliconflow() {
        assert!(create_model_provider("siliconflow", Some("key")).is_ok());
    }

    #[test]
    fn factory_codex_dispatches_via_requires_openai_auth_flag() {
        // Codex selection: the typed alias's `base.requires_openai_auth`
        // routes through `OpenAIModelProviderConfig::create_model_provider`. The
        // legacy escape hatch on the bare "openai-codex" / "openai_codex" /
        // "codex" family names remains for callers without Config context
        // (this test).
        let options = ModelProviderRuntimeOptions::default();
        assert!(create_model_provider_with_options("openai-codex", None, &options).is_ok());
    }

    #[test]
    fn factory_atomic_chat() {
        assert!(create_model_provider("atomic_chat", Some("key")).is_ok());
    }

    #[test]
    fn factory_atomic_chat_allows_missing_key() {
        // Local provider — empty key is acceptable; the runtime still
        // attaches a placeholder Bearer header.
        assert!(create_model_provider("atomic_chat", None).is_ok());
    }

    #[test]
    fn atomic_chat_is_listed_as_local_provider() {
        let providers = list_model_providers();
        let provider = providers
            .iter()
            .find(|p| p.name == "atomic_chat")
            .expect("atomic_chat must be listed");
        assert!(provider.local, "atomic_chat must be a local provider");
    }

    // ── Extended ecosystem ───────────────────────────────────

    #[test]
    fn factory_groq() {
        assert!(create_model_provider("groq", Some("key")).is_ok());
    }

    #[test]
    fn factory_groq_disables_native_tools_by_default() {
        // Default behavior preserves the blanket disable: llama-family
        // Groq models reject native tool calls with HTTP 400.
        let model_provider = create_model_provider_with_options(
            "groq",
            Some("key"),
            &ModelProviderRuntimeOptions::default(),
        )
        .expect("groq factory must succeed");
        assert!(
            !model_provider.supports_native_tools(),
            "Groq must default to text-fallback for llama-family compatibility"
        );
    }

    #[test]
    fn factory_groq_honors_native_tools_override_true() {
        // Operator opt-in via `[model_providers.groq.<alias>] native_tools = true`
        // skips the default disable so non-llama Groq models can use native
        // tool calling.
        let options = ModelProviderRuntimeOptions {
            native_tools: Some(true),
            ..Default::default()
        };
        let model_provider = create_model_provider_with_options("groq", Some("key"), &options)
            .expect("groq factory must succeed");
        assert!(
            model_provider.supports_native_tools(),
            "Groq with `native_tools = true` must enable native tool calling"
        );
    }

    #[test]
    fn factory_groq_native_tools_override_false_keeps_disable() {
        // Explicit `native_tools = false` matches the default behavior; this
        // documents that the option is tri-state and `Some(false)` is not a
        // no-op surprise.
        let options = ModelProviderRuntimeOptions {
            native_tools: Some(false),
            ..Default::default()
        };
        let model_provider = create_model_provider_with_options("groq", Some("key"), &options)
            .expect("groq factory must succeed");
        assert!(
            !model_provider.supports_native_tools(),
            "Groq with explicit `native_tools = false` must remain text-fallback"
        );
    }

    #[test]
    fn provider_runtime_options_from_config_propagates_native_tools() {
        // End-to-end path: setting `native_tools` on the first configured
        // model_provider entry must reach `ModelProviderRuntimeOptions` so the
        // Groq factory branch sees it. There is no global fallback; the
        // orchestrator resolves per-agent and falls back to
        // `first_model_provider()`.
        use zeroclaw_config::schema::{GroqModelProviderConfig, ModelProviderConfig};
        let mut config = zeroclaw_config::schema::Config::default();
        config.providers.models.groq.insert(
            "default".to_string(),
            GroqModelProviderConfig {
                base: ModelProviderConfig {
                    uri: Some("https://api.groq.com/openai/v1".to_string()),
                    native_tools: Some(true),
                    ..Default::default()
                },
            },
        );

        let options = provider_runtime_options_from_config(&config);
        assert_eq!(
            options.native_tools,
            Some(true),
            "native_tools must propagate from the active model_provider entry to runtime options"
        );
    }

    #[test]
    fn factory_mistral() {
        assert!(create_model_provider("mistral", Some("key")).is_ok());
    }

    #[test]
    fn factory_xai() {
        assert!(create_model_provider("xai", Some("key")).is_ok());
    }

    #[test]
    fn factory_deepseek() {
        assert!(create_model_provider("deepseek", Some("key")).is_ok());
    }

    #[test]
    fn deepseek_provider_keeps_vision_disabled() {
        let model_provider = create_model_provider("deepseek", Some("key"))
            .expect("deepseek model_provider should build");
        assert!(!model_provider.supports_vision());
    }

    #[test]
    fn factory_together() {
        assert!(create_model_provider("together", Some("key")).is_ok());
    }

    #[test]
    fn factory_fireworks() {
        assert!(create_model_provider("fireworks", Some("key")).is_ok());
    }

    #[test]
    fn factory_novita() {
        assert!(create_model_provider("novita", Some("key")).is_ok());
    }

    #[test]
    fn factory_perplexity() {
        assert!(create_model_provider("perplexity", Some("key")).is_ok());
    }

    #[test]
    fn factory_cohere() {
        assert!(create_model_provider("cohere", Some("key")).is_ok());
    }

    #[test]
    fn factory_copilot() {
        assert!(create_model_provider("copilot", Some("key")).is_ok());
    }

    #[test]
    fn factory_gemini_cli() {}

    #[test]
    fn factory_kilocli() {
        assert!(create_model_provider("kilocli", None).is_ok());
    }

    #[test]
    fn factory_nvidia() {
        assert!(create_model_provider("nvidia", Some("nvapi-test")).is_ok());
    }

    // ── AI inference routers ─────────────────────────────────

    #[test]
    fn factory_astrai() {
        assert!(create_model_provider("astrai", Some("sk-astrai-test")).is_ok());
    }

    #[test]
    fn factory_avian() {
        assert!(create_model_provider("avian", Some("sk-avian-test")).is_ok());
    }

    #[test]
    fn factory_deepmyst() {
        assert!(create_model_provider("deepmyst", Some("key")).is_ok());
    }

    #[test]
    fn resolve_provider_credential_deepmyst_env_deleted() {}

    // ── Custom / BYOP model model_provider ─────────────────────────
    //
    // The legacy colon-URL form ("custom:https://..." / "anthropic-custom:...")
    // and its in-process URL parser were deleted in #6273. The surface is
    // `[model_providers.custom.<alias>] uri = "https://..."` for OpenAI-
    // compatible endpoints (or `[model_providers.anthropic.<alias>] uri = ...`
    // for Anthropic-compatible). URL validation now happens at schema-load
    // time in `crates/zeroclaw-config/src/schema.rs::validate`, not at runtime
    // construction; tests for that validation belong with the schema, not here.
    //
    // Migration of legacy colon-URL configs is exercised by the integration
    // tests in `crates/zeroclaw-config/tests/migration.rs`
    // (`anthropic_custom_colon_url_default_provider_folds_under_anthropic`,
    // `custom_colon_url_default_provider_splits_into_uri`,
    // `agent_inline_brain_colon_url_provider_splits_into_uri`).

    #[test]
    fn factory_custom_with_resolved_uri() {
        let options = ModelProviderRuntimeOptions {
            provider_api_url: Some("https://my-llm.example.com".to_string()),
            ..ModelProviderRuntimeOptions::default()
        };
        assert!(create_model_provider_with_options("custom", Some("key"), &options).is_ok());
    }

    #[test]
    fn factory_custom_without_uri_errors() {
        match create_model_provider("custom", Some("key")) {
            Err(e) => assert!(
                e.to_string().contains("requires `uri`"),
                "Expected `uri` error, got: {e}"
            ),
            Ok(_) => {
                panic!("Expected error when custom model model_provider has no URI configured")
            }
        }
    }

    // ── Error cases ──────────────────────────────────────────

    #[test]
    fn factory_unknown_provider_errors() {
        let p = create_model_provider("nonexistent", None);
        assert!(p.is_err());
        let msg = p.err().unwrap().to_string();
        assert!(msg.contains("Unknown model_provider family"));
        assert!(msg.contains("nonexistent"));
    }

    #[test]
    fn factory_empty_name_errors() {
        assert!(create_model_provider("", None).is_err());
    }

    #[test]
    fn ollama_with_custom_url() {
        let model_provider =
            create_model_provider_with_url("ollama", None, Some("http://10.100.2.32:11434"));
        assert!(model_provider.is_ok());
    }

    #[test]
    fn ollama_cloud_with_custom_url() {
        let model_provider = create_model_provider_with_url(
            "ollama",
            Some("ollama-key"),
            Some("https://ollama.com"),
        );
        assert!(model_provider.is_ok());
    }

    #[tokio::test]
    async fn ollama_private_remote_cloud_request_omits_auth_and_preserves_model() {
        use axum::{
            Json, Router,
            extract::State,
            http::{HeaderMap, StatusCode},
            routing::post,
        };
        use serde_json::{Value, json};
        use std::sync::{Arc, Mutex};

        type Capture = Arc<Mutex<Option<(Option<String>, String)>>>;

        async fn capture_chat_request(
            State(capture): State<Capture>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> (StatusCode, Json<Value>) {
            let auth = headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let model = body
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            *capture.lock().expect("capture lock poisoned") = Some((auth, model));
            (
                StatusCode::OK,
                Json(json!({
                    "choices": [{"message": {"content": "ok"}}]
                })),
            )
        }

        let capture: Capture = Arc::new(Mutex::new(None));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("test server addr");
        let app = Router::new()
            .route("/v1/chat/completions", post(capture_chat_request))
            .with_state(capture.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test server");
        });

        let base_url = format!("http://{addr}");
        let model_provider = create_model_provider_with_url("ollama", None, Some(&base_url))
            .expect("ollama provider should build");
        let response = model_provider
            .chat_with_system(None, "hello", "qwen3:cloud", Some(0.7))
            .await
            .expect("chat request should succeed");

        assert_eq!(response, "ok");
        let (auth, model) = capture
            .lock()
            .expect("capture lock poisoned")
            .take()
            .expect("server should capture request");
        assert_eq!(auth, None);
        assert_eq!(model, "qwen3:cloud");
        server.abort();
    }

    #[tokio::test]
    async fn ollama_private_remote_lists_models_without_auth() {
        use axum::{Json, Router, extract::State, http::HeaderMap, routing::get};
        use serde_json::{Value, json};
        use std::sync::{Arc, Mutex};

        type Capture = Arc<Mutex<Option<Option<String>>>>;

        async fn capture_models_request(
            State(capture): State<Capture>,
            headers: HeaderMap,
        ) -> Json<Value> {
            let auth = headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            *capture.lock().expect("capture lock poisoned") = Some(auth);
            Json(json!({
                "data": [{"id": "qwen3:cloud"}]
            }))
        }

        let capture: Capture = Arc::new(Mutex::new(None));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("test server addr");
        let app = Router::new()
            .route("/v1/models", get(capture_models_request))
            .with_state(capture.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test server");
        });

        let base_url = format!("http://{addr}");
        let model_provider = create_model_provider_with_url("ollama", None, Some(&base_url))
            .expect("ollama provider should build");
        let models = model_provider
            .list_models()
            .await
            .expect("model list should succeed");

        assert_eq!(models, vec!["qwen3:cloud".to_string()]);
        let auth = capture
            .lock()
            .expect("capture lock poisoned")
            .take()
            .expect("server should capture request");
        assert_eq!(auth, None);
        server.abort();
    }

    #[test]
    fn factory_all_canonical_model_providers_create_successfully() {
        // Canonical family names only — legacy synonyms are collapsed by
        // `normalize_model_provider_type` in `schema/v2.rs` and never reach
        // the runtime. `azure` is excluded (typed-config required, see
        // `listed_model_providers_are_constructible` skip list); `custom` is
        // excluded (URI required, covered by `factory_custom_*` tests).
        let canonical = [
            "openrouter",
            "anthropic",
            "openai",
            "ollama",
            "gemini",
            "venice",
            "vercel",
            "cloudflare",
            "moonshot",
            "synthetic",
            "opencode",
            "zai",
            "glm",
            "minimax",
            "bedrock",
            "qianfan",
            "doubao",
            "qwen",
            "lmstudio",
            "llamacpp",
            "sglang",
            "vllm",
            "osaurus",
            "telnyx",
            "groq",
            "mistral",
            "xai",
            "deepseek",
            "together",
            "fireworks",
            "novita",
            "perplexity",
            "cohere",
            "copilot",
            "gemini_cli",
            "kilocli",
            "nvidia",
            "astrai",
            "avian",
            "ovh",
        ];
        for name in canonical {
            assert!(
                create_model_provider(name, Some("test-key")).is_ok(),
                "Canonical model model_provider '{name}' should create successfully"
            );
        }
    }

    #[test]
    fn listed_model_providers_have_unique_canonical_ids() {
        let model_providers = list_model_providers();
        let mut canonical_ids = std::collections::HashSet::new();

        for model_provider in model_providers {
            assert!(
                canonical_ids.insert(model_provider.name),
                "Duplicate canonical model model_provider id: {}",
                model_provider.name
            );
        }
    }

    #[test]
    fn listed_model_providers_are_constructible() {
        for model_provider in list_model_providers() {
            // Azure requires typed config (resource + deployment) per #6273.
            // create_model_provider with default options has no azure context — that's
            // by design (env-var fallback eradicated). Tests that exercise the
            // Azure factory pass a populated ModelProviderRuntimeOptions through
            // create_model_provider_with_options.
            if model_provider.name == "azure" {
                continue;
            }
            // The custom slot requires a uri (no family-default endpoint);
            // covered by dedicated factory tests.
            if model_provider.name == "custom" {
                continue;
            }
            assert!(
                create_model_provider(model_provider.name, Some("provider-test-credential"))
                    .is_ok(),
                "Canonical model model_provider id should be constructible: {}",
                model_provider.name
            );
        }
    }

    // ── API error sanitization ───────────────────────────────

    #[test]
    fn format_error_chain_includes_sources_and_sanitizes_output() {
        #[derive(Debug)]
        struct ChainError {
            message: &'static str,
            source: Option<Box<dyn std::error::Error + 'static>>,
        }

        impl std::fmt::Display for ChainError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.message)
            }
        }

        impl std::error::Error for ChainError {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                self.source.as_deref()
            }
        }

        let error = ChainError {
            message: "outer context",
            source: Some(Box::new(ChainError {
                message: "middle context",
                source: Some(Box::new(ChainError {
                    message: "inner source leaked sk-1234567890abcdef",
                    source: None,
                })),
            })),
        };

        let result = format_error_chain(&error);

        assert!(result.contains("outer context"));
        assert!(result.contains("middle context"));
        assert!(result.contains("inner source leaked [REDACTED]"));
        assert!(!result.contains("sk-1234567890abcdef"));
    }

    #[test]
    fn sanitize_scrubs_sk_prefix() {
        let input = "request failed: sk-1234567890abcdef";
        let out = sanitize_api_error(input);
        assert!(!out.contains("sk-1234567890abcdef"));
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn sanitize_scrubs_multiple_prefixes() {
        let input = "keys sk-abcdef xoxb-12345 xoxp-67890";
        let out = sanitize_api_error(input);
        assert!(!out.contains("sk-abcdef"));
        assert!(!out.contains("xoxb-12345"));
        assert!(!out.contains("xoxp-67890"));
    }

    #[test]
    fn sanitize_short_prefix_then_real_key() {
        let input = "error with sk- prefix and key sk-1234567890";
        let result = sanitize_api_error(input);
        assert!(!result.contains("sk-1234567890"));
        assert!(result.contains("[REDACTED]"));
    }

    #[test]
    fn sanitize_sk_proj_comment_then_real_key() {
        let input = "note: sk- then sk-proj-abc123def456";
        let result = sanitize_api_error(input);
        assert!(!result.contains("sk-proj-abc123def456"));
        assert!(result.contains("[REDACTED]"));
    }

    #[test]
    fn sanitize_keeps_bare_prefix() {
        let input = "only prefix sk- present";
        let result = sanitize_api_error(input);
        assert!(result.contains("sk-"));
    }

    #[test]
    fn sanitize_handles_json_wrapped_key() {
        let input = r#"{"error":"invalid key sk-abc123xyz"}"#;
        let result = sanitize_api_error(input);
        assert!(!result.contains("sk-abc123xyz"));
    }

    #[test]
    fn sanitize_handles_delimiter_boundaries() {
        let input = "bad token xoxb-abc123}; next";
        let result = sanitize_api_error(input);
        assert!(!result.contains("xoxb-abc123"));
        assert!(result.contains("};"));
    }

    #[test]
    fn sanitize_truncates_long_error() {
        let long = "a".repeat(600);
        let result = sanitize_api_error(&long);
        assert!(result.len() <= 503);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn sanitize_truncates_after_scrub() {
        let input = format!("{} sk-abcdef123456 {}", "a".repeat(290), "b".repeat(290));
        let result = sanitize_api_error(&input);
        assert!(!result.contains("sk-abcdef123456"));
        assert!(result.len() <= 503);
    }

    #[test]
    fn sanitize_preserves_unicode_boundaries() {
        let input = format!("{} sk-abcdef123", "hello🙂".repeat(80));
        let result = sanitize_api_error(&input);
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
        assert!(!result.contains("sk-abcdef123"));
    }

    #[test]
    fn sanitize_no_secret_no_change() {
        let input = "simple upstream timeout";
        let result = sanitize_api_error(input);
        assert_eq!(result, input);
    }

    #[test]
    fn scrub_github_personal_access_token() {
        let input = "auth failed with token ghp_abc123def456";
        let result = scrub_secret_patterns(input);
        assert_eq!(result, "auth failed with token [REDACTED]");
    }

    #[test]
    fn scrub_github_oauth_token() {
        let input = "Bearer gho_1234567890abcdef";
        let result = scrub_secret_patterns(input);
        assert_eq!(result, "Bearer [REDACTED]");
    }

    #[test]
    fn scrub_github_user_token() {
        let input = "token ghu_sessiontoken123";
        let result = scrub_secret_patterns(input);
        assert_eq!(result, "token [REDACTED]");
    }

    #[test]
    fn scrub_github_fine_grained_pat() {
        let input = "failed: github_pat_11AABBC_xyzzy789";
        let result = scrub_secret_patterns(input);
        assert_eq!(result, "failed: [REDACTED]");
    }

    // ── API key prefix pre-flight ───────────────────────────

    #[test]
    fn api_key_prefix_cross_provider_mismatch() {
        // Anthropic key used with openrouter
        assert_eq!(
            check_api_key_prefix("openrouter", "sk-ant-api03-xyz"),
            Some("anthropic")
        );
        // OpenRouter key used with anthropic
        assert_eq!(
            check_api_key_prefix("anthropic", "sk-or-v1-xyz"),
            Some("openrouter")
        );
        // Anthropic key used with openai
        assert_eq!(
            check_api_key_prefix("openai", "sk-ant-xyz"),
            Some("anthropic")
        );
        // Groq key used with openai
        assert_eq!(check_api_key_prefix("openai", "gsk_xyz"), Some("groq"));
    }

    #[test]
    fn api_key_prefix_correct_match() {
        assert_eq!(check_api_key_prefix("anthropic", "sk-ant-api03-xyz"), None);
        assert_eq!(check_api_key_prefix("openrouter", "sk-or-v1-xyz"), None);
        assert_eq!(check_api_key_prefix("openai", "sk-proj-xyz"), None);
        assert_eq!(check_api_key_prefix("groq", "gsk_xyz"), None);
    }

    #[test]
    fn api_key_prefix_unknown_provider_skips() {
        // Providers without known key formats should never flag a mismatch.
        assert_eq!(check_api_key_prefix("deepseek", "sk-ant-xyz"), None);
        assert_eq!(check_api_key_prefix("ollama", "anything"), None);
    }

    #[test]
    fn api_key_prefix_unknown_key_format_skips() {
        // Keys without a recognisable prefix should never flag a mismatch.
        assert_eq!(check_api_key_prefix("openai", "my-custom-key-123"), None);
        assert_eq!(check_api_key_prefix("anthropic", "some-random-key"), None);
    }

    #[test]
    fn provider_runtime_options_default_has_empty_extra_headers() {
        let options = ModelProviderRuntimeOptions::default();
        assert!(options.extra_headers.is_empty());
    }

    #[test]
    fn provider_runtime_options_extra_headers_passed_through() {
        let mut extra_headers = std::collections::HashMap::new();
        extra_headers.insert("X-Title".to_string(), "zeroclaw".to_string());
        let options = ModelProviderRuntimeOptions {
            extra_headers,
            ..ModelProviderRuntimeOptions::default()
        };
        assert_eq!(options.extra_headers.len(), 1);
        assert_eq!(options.extra_headers.get("X-Title").unwrap(), "zeroclaw");
    }

    #[test]
    fn ollama_uses_resolved_url_from_runtime_options() {
        // V0.8.0: `ZEROCLAW_PROVIDER_URL` env-var override eradicated. Ollama
        // base URL flows through the typed alias's `api_url`/`uri` field which
        // pre-populates `provider_api_url` on `ModelProviderRuntimeOptions`.
        let model_provider =
            create_model_provider_with_url("ollama", None, Some("http://config-ollama:11434"));
        assert!(model_provider.is_ok());
    }

    // ── Per-alias provider_runtime_options resolution ──

    /// Build a `Config` with two `anthropic` aliases at different base_urls
    /// so the test can prove `provider_runtime_options_for_agent` selects
    /// the alias-specific entry rather than `first_model_provider()`.
    fn config_with_two_anthropic_aliases() -> zeroclaw_config::schema::Config {
        use zeroclaw_config::schema::{
            AliasedAgentConfig, AnthropicModelProviderConfig, Config, ModelProviderConfig,
        };
        let mut config = Config::default();
        let default_alias = AnthropicModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("claude-default".into()),
                api_key: Some("default-key".into()),
                uri: Some("https://api.default.example/v1/messages".into()),
                ..ModelProviderConfig::default()
            },
        };
        let work_alias = AnthropicModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("claude-work".into()),
                api_key: Some("work-key".into()),
                uri: Some("https://work-proxy.example/v1/v1/anthropic/messages".into()),
                ..ModelProviderConfig::default()
            },
        };
        config
            .providers
            .models
            .anthropic
            .insert("default".to_string(), default_alias);
        config
            .providers
            .models
            .anthropic
            .insert("work".to_string(), work_alias);
        let work_agent = AliasedAgentConfig {
            model_provider: "anthropic.work".into(),
            ..AliasedAgentConfig::default()
        };
        config.agents.insert("work_agent".to_string(), work_agent);
        let default_agent = AliasedAgentConfig {
            model_provider: "anthropic.default".into(),
            ..AliasedAgentConfig::default()
        };
        config
            .agents
            .insert("default_agent".to_string(), default_agent);
        config
    }

    #[test]
    fn provider_runtime_options_for_agent_resolves_alias_specific_uri() {
        let config = config_with_two_anthropic_aliases();
        let work = provider_runtime_options_for_agent(&config, "work_agent");
        let dflt = provider_runtime_options_for_agent(&config, "default_agent");

        assert_eq!(
            work.provider_api_url.as_deref(),
            Some("https://work-proxy.example/v1/v1/anthropic/messages"),
            "work agent must resolve to the work alias's full uri (with merged path)"
        );
        assert_eq!(
            dflt.provider_api_url.as_deref(),
            Some("https://api.default.example/v1/messages"),
            "default agent must resolve to the default alias's full uri (with merged path)"
        );
    }

    #[test]
    fn provider_runtime_options_for_agent_falls_back_to_first_provider_when_unknown_agent() {
        let config = config_with_two_anthropic_aliases();
        let opts = provider_runtime_options_for_agent(&config, "nonexistent");
        // Falls back to first_model_provider() — order across HashMap is not
        // guaranteed but the URL must match one of the configured aliases.
        let url = opts.provider_api_url.as_deref().unwrap_or("");
        assert!(
            url == "https://work-proxy.example/v1/v1/anthropic/messages"
                || url == "https://api.default.example/v1/messages",
            "fallback must resolve to one of the configured anthropic aliases; got `{url}`"
        );
    }

    #[test]
    fn ollama_alias_tuning_fields_populate_tuning_struct() {
        let alias = zeroclaw_config::schema::OllamaModelProviderConfig {
            num_ctx: Some(16384),
            num_predict: Some(4096),
            temperature_override: Some(0.5),
            ..zeroclaw_config::schema::OllamaModelProviderConfig::default()
        };

        let tuning = ollama::OllamaTuning::from_runtime_overrides(
            alias.num_ctx,
            alias.num_predict,
            alias.temperature_override,
        );
        assert_eq!(tuning.num_ctx, 16384);
        assert_eq!(tuning.num_predict, 4096);
        assert_eq!(tuning.temperature_override, Some(0.5));

        let provider = ollama::OllamaModelProvider::new("test", None, None).with_tuning(tuning);
        assert_eq!(provider.tuning(), tuning);
    }

    #[test]
    fn ollama_alias_tuning_defaults_leave_temperature_override_unset() {
        let alias = zeroclaw_config::schema::OllamaModelProviderConfig::default();
        let tuning = ollama::OllamaTuning::from_runtime_overrides(
            alias.num_ctx,
            alias.num_predict,
            alias.temperature_override,
        );
        assert!(tuning.temperature_override.is_none());
        assert_eq!(tuning.num_ctx, ollama::OLLAMA_DEFAULT_NUM_CTX);
        assert_eq!(tuning.num_predict, ollama::OLLAMA_DEFAULT_NUM_PREDICT);
    }

    fn config_with_openai_alias() -> zeroclaw_config::schema::Config {
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, ModelProviderConfig, OpenAIModelProviderConfig,
        };
        let mut config = Config::default();
        let alias = OpenAIModelProviderConfig {
            base: ModelProviderConfig {
                api_key: Some("openai-alias-key".into()),
                model: Some("gpt-4o".into()),
                ..ModelProviderConfig::default()
            },
        };
        config
            .providers
            .models
            .openai
            .insert("alias".to_string(), alias);
        let agent = AliasedAgentConfig {
            model_provider: "openai.alias".into(),
            ..AliasedAgentConfig::default()
        };
        config.agents.insert("test_agent".to_string(), agent);
        config
    }

    #[test]
    fn routed_model_provider_credential_precedence_uses_route_key_first() {
        let config = config_with_openai_alias();
        let reliability = zeroclaw_config::schema::ReliabilityConfig::default();
        let routes = [zeroclaw_config::schema::ModelRouteConfig {
            hint: "test".into(),
            model_provider: "openai.alias".into(),
            model: "gpt-4o".into(),
            api_key: Some("route-key".into()),
        }];

        let result = create_routed_model_provider_with_options(
            &config,
            "openai.alias",
            Some("fallback-key"),
            None,
            &reliability,
            &routes,
            "gpt-4o",
            &ModelProviderRuntimeOptions::default(),
        );

        assert!(
            result.is_ok(),
            "route-key should succeed: {}",
            result.err().unwrap()
        );
    }

    #[test]
    fn routed_model_provider_credential_precedence_uses_config_entry_key() {
        let config = config_with_openai_alias();
        let reliability = zeroclaw_config::schema::ReliabilityConfig::default();
        // Route has no api_key — should fall back to config entry key "openai-alias-key"
        let routes = [zeroclaw_config::schema::ModelRouteConfig {
            hint: "test".into(),
            model_provider: "openai.alias".into(),
            model: "gpt-4o".into(),
            api_key: None,
        }];

        let result = create_routed_model_provider_with_options(
            &config,
            "openai.alias",
            Some("fallback-key"),
            None,
            &reliability,
            &routes,
            "gpt-4o",
            &ModelProviderRuntimeOptions::default(),
        );

        assert!(
            result.is_ok(),
            "config-entry key should succeed: {}",
            result.err().unwrap()
        );
    }

    #[test]
    fn routed_model_provider_credential_precedence_falls_back_to_api_key_param() {
        let config = zeroclaw_config::schema::Config::default(); // no entry in config.models
        let reliability = zeroclaw_config::schema::ReliabilityConfig::default();
        // Neither route nor config entry has api_key — should use the param "fallback-key"
        let routes = [zeroclaw_config::schema::ModelRouteConfig {
            hint: "test".into(),
            model_provider: "openai".into(),
            model: "gpt-4o".into(),
            api_key: None,
        }];

        let result = create_routed_model_provider_with_options(
            &config,
            "openai",
            Some("fallback-key"),
            None,
            &reliability,
            &routes,
            "gpt-4o",
            &ModelProviderRuntimeOptions::default(),
        );

        assert!(
            result.is_ok(),
            "fallback-key should succeed: {}",
            result.err().unwrap()
        );
    }

    #[test]
    fn routed_model_provider_credential_skips_config_entry_for_non_dotted_name() {
        let config = zeroclaw_config::schema::Config::default();
        let reliability = zeroclaw_config::schema::ReliabilityConfig::default();
        // Non-dotted name "openai" — split_once('.') returns None, so config entry
        // lookup is skipped entirely. Falls back to api_key param.
        let routes = [zeroclaw_config::schema::ModelRouteConfig {
            hint: "test".into(),
            model_provider: "openai".into(),
            model: "gpt-4o".into(),
            api_key: None,
        }];

        let result = create_routed_model_provider_with_options(
            &config,
            "openai",
            Some("direct-key"),
            None,
            &reliability,
            &routes,
            "gpt-4o",
            &ModelProviderRuntimeOptions::default(),
        );

        assert!(
            result.is_ok(),
            "direct-key should succeed: {}",
            result.err().unwrap()
        );
    }
}
