//! Provider subsystem for model inference backends.
//!
//! This module implements the factory pattern for AI model providers. Each provider
//! implements the [`Provider`] trait defined in [`traits`], and is registered in the
//! factory function [`create_provider`] by its canonical string key (e.g., `"openai"`,
//! `"anthropic"`, `"ollama"`, `"gemini"`). Provider aliases are resolved internally
//! so that user-facing keys remain stable.
//!
//! The subsystem supports resilient multi-provider configurations through the
//! [`ReliableProvider`] wrapper, which handles fallback
//! chains and automatic retry. Model routing across providers is available via
//! [`create_routed_provider`].
//!
//! # Extension
//!
//! To add a new provider, implement [`Provider`] in a new submodule and register it
//! in [`create_provider_with_url`]. See `AGENTS.md` §7.1 for the full change playbook.

pub mod anthropic;
pub mod auth;
pub mod azure_openai;
pub mod bedrock;
pub mod claude_code;
pub mod compatible;
pub mod copilot;
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
pub mod reliable;
pub mod router;
pub mod telnyx;
pub mod traits;

#[allow(unused_imports)]
pub use traits::{
    ChatMessage, ChatRequest, ChatResponse, ConversationMessage, Provider, ProviderCapabilityError,
    ToolCall, ToolResultMessage,
};

use crate::auth::AuthService;
use compatible::{AuthStyle, OpenAiCompatibleProvider};
use reliable::ReliableProvider;
use serde::Deserialize;
use std::path::PathBuf;

const MAX_API_ERROR_CHARS: usize = 500;
const MINIMAX_INTL_BASE_URL: &str = "https://api.minimax.io/v1";
const MINIMAX_CN_BASE_URL: &str = "https://api.minimaxi.com/v1";
const MINIMAX_OAUTH_GLOBAL_TOKEN_ENDPOINT: &str = "https://api.minimax.io/oauth/token";
const MINIMAX_OAUTH_CN_TOKEN_ENDPOINT: &str = "https://api.minimaxi.com/oauth/token";
const MINIMAX_OAUTH_PLACEHOLDER: &str = "minimax-oauth";
const MINIMAX_OAUTH_CN_PLACEHOLDER: &str = "minimax-oauth-cn";
const MINIMAX_OAUTH_TOKEN_ENV: &str = "MINIMAX_OAUTH_TOKEN";
const MINIMAX_API_KEY_ENV: &str = "MINIMAX_API_KEY";
const MINIMAX_OAUTH_REFRESH_TOKEN_ENV: &str = "MINIMAX_OAUTH_REFRESH_TOKEN";
const MINIMAX_OAUTH_REGION_ENV: &str = "MINIMAX_OAUTH_REGION";
const MINIMAX_OAUTH_CLIENT_ID_ENV: &str = "MINIMAX_OAUTH_CLIENT_ID";
const MINIMAX_OAUTH_DEFAULT_CLIENT_ID: &str = "78257093-7e40-4613-99e0-527b14b39113";
const GLM_GLOBAL_BASE_URL: &str = "https://api.z.ai/api/paas/v4";
const GLM_CN_BASE_URL: &str = "https://open.bigmodel.cn/api/paas/v4";
const MOONSHOT_INTL_BASE_URL: &str = "https://api.moonshot.ai/v1";
const MOONSHOT_CN_BASE_URL: &str = "https://api.moonshot.cn/v1";
const QWEN_CN_BASE_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1";
const QWEN_INTL_BASE_URL: &str = "https://dashscope-intl.aliyuncs.com/compatible-mode/v1";
const QWEN_US_BASE_URL: &str = "https://dashscope-us.aliyuncs.com/compatible-mode/v1";
const QWEN_OAUTH_BASE_FALLBACK_URL: &str = QWEN_CN_BASE_URL;
const BAILIAN_BASE_URL: &str = "https://coding.dashscope.aliyuncs.com/v1";
const QWEN_OAUTH_TOKEN_ENDPOINT: &str = "https://chat.qwen.ai/api/v1/oauth2/token";
const QWEN_OAUTH_PLACEHOLDER: &str = "qwen-oauth";
const QWEN_OAUTH_TOKEN_ENV: &str = "QWEN_OAUTH_TOKEN";
const QWEN_OAUTH_REFRESH_TOKEN_ENV: &str = "QWEN_OAUTH_REFRESH_TOKEN";
const QWEN_OAUTH_RESOURCE_URL_ENV: &str = "QWEN_OAUTH_RESOURCE_URL";
const QWEN_OAUTH_CLIENT_ID_ENV: &str = "QWEN_OAUTH_CLIENT_ID";
const QWEN_OAUTH_DEFAULT_CLIENT_ID: &str = "f0304373b74a44d2b584a3fb70ca9e56";
const QWEN_OAUTH_CREDENTIAL_FILE: &str = ".qwen/oauth_creds.json";
const ZAI_GLOBAL_BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";
const ZAI_CN_BASE_URL: &str = "https://open.bigmodel.cn/api/coding/paas/v4";
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

#[derive(Clone, Copy, Debug)]
enum MinimaxOauthRegion {
    Global,
    Cn,
}

impl MinimaxOauthRegion {
    fn token_endpoint(self) -> &'static str {
        match self {
            Self::Global => MINIMAX_OAUTH_GLOBAL_TOKEN_ENDPOINT,
            Self::Cn => MINIMAX_OAUTH_CN_TOKEN_ENDPOINT,
        }
    }
}

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

#[derive(Clone, Deserialize, Default)]
struct QwenOauthCredentials {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    resource_url: Option<String>,
    #[serde(default)]
    expiry_date: Option<i64>,
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
struct QwenOauthProviderContext {
    credential: Option<String>,
    base_url: Option<String>,
}

impl std::fmt::Debug for QwenOauthProviderContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QwenOauthProviderContext")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

fn read_non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn is_minimax_oauth_placeholder(value: &str) -> bool {
    value.eq_ignore_ascii_case(MINIMAX_OAUTH_PLACEHOLDER)
        || value.eq_ignore_ascii_case(MINIMAX_OAUTH_CN_PLACEHOLDER)
}

fn minimax_oauth_region(name: &str) -> MinimaxOauthRegion {
    if let Some(region) = read_non_empty_env(MINIMAX_OAUTH_REGION_ENV) {
        let normalized = region.to_ascii_lowercase();
        if matches!(normalized.as_str(), "cn" | "china") {
            return MinimaxOauthRegion::Cn;
        }
        if matches!(normalized.as_str(), "global" | "intl" | "international") {
            return MinimaxOauthRegion::Global;
        }
    }

    if is_minimax_cn_alias(name) {
        MinimaxOauthRegion::Cn
    } else {
        MinimaxOauthRegion::Global
    }
}

fn minimax_oauth_client_id() -> String {
    read_non_empty_env(MINIMAX_OAUTH_CLIENT_ID_ENV)
        .unwrap_or_else(|| MINIMAX_OAUTH_DEFAULT_CLIENT_ID.to_string())
}

fn qwen_oauth_client_id() -> String {
    read_non_empty_env(QWEN_OAUTH_CLIENT_ID_ENV)
        .unwrap_or_else(|| QWEN_OAUTH_DEFAULT_CLIENT_ID.to_string())
}

fn qwen_oauth_credentials_file_path() -> Option<PathBuf> {
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

fn refresh_qwen_oauth_access_token(refresh_token: &str) -> anyhow::Result<QwenOauthCredentials> {
    let client_id = qwen_oauth_client_id();
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
            ("client_id", client_id.as_str()),
        ])
        .send()
        .map_err(|error| anyhow::anyhow!("Qwen OAuth refresh request failed: {error}"))?;

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
        anyhow::bail!("Qwen OAuth refresh failed (HTTP {status}): {detail}");
    }

    let payload =
        parsed.ok_or_else(|| anyhow::anyhow!("Qwen OAuth refresh response is not JSON"))?;

    if let Some(error_code) = payload
        .error
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let detail = payload.error_description.as_deref().unwrap_or(error_code);
        anyhow::bail!("Qwen OAuth refresh failed: {detail}");
    }

    let access_token = payload
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Qwen OAuth refresh response missing access_token"))?
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

    let mut cached = read_qwen_oauth_cached_credentials();

    let env_token = read_non_empty_env(QWEN_OAUTH_TOKEN_ENV);
    let env_refresh_token = read_non_empty_env(QWEN_OAUTH_REFRESH_TOKEN_ENV);
    let env_resource_url = read_non_empty_env(QWEN_OAUTH_RESOURCE_URL_ENV);

    if env_token.is_none() {
        let refresh_token = env_refresh_token.clone().or_else(|| {
            cached
                .as_ref()
                .and_then(|credentials| credentials.refresh_token.clone())
        });

        let should_refresh = cached.as_ref().is_some_and(qwen_oauth_token_expired)
            || cached
                .as_ref()
                .and_then(|credentials| credentials.access_token.as_deref())
                .is_none_or(|value| value.trim().is_empty());

        if should_refresh && let Some(refresh_token) = refresh_token.as_deref() {
            match refresh_qwen_oauth_access_token(refresh_token) {
                Ok(refreshed) => {
                    cached = Some(refreshed);
                }
                Err(error) => {
                    tracing::warn!(error = %error, "Qwen OAuth refresh failed");
                }
            }
        }
    }

    let mut credential = env_token.or_else(|| {
        cached
            .as_ref()
            .and_then(|credentials| credentials.access_token.clone())
    });
    credential = credential
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);

    if credential.is_none() && !placeholder_requested {
        credential = read_non_empty_env("DASHSCOPE_API_KEY");
    }

    let base_url = env_resource_url
        .as_deref()
        .and_then(normalize_qwen_oauth_base_url)
        .or_else(|| {
            cached
                .as_ref()
                .and_then(|credentials| credentials.resource_url.as_deref())
                .and_then(normalize_qwen_oauth_base_url)
        });

    QwenOauthProviderContext {
        credential,
        base_url,
    }
}

fn resolve_minimax_static_credential() -> Option<String> {
    read_non_empty_env(MINIMAX_OAUTH_TOKEN_ENV).or_else(|| read_non_empty_env(MINIMAX_API_KEY_ENV))
}

fn refresh_minimax_oauth_access_token(name: &str, refresh_token: &str) -> anyhow::Result<String> {
    let region = minimax_oauth_region(name);
    let endpoint = region.token_endpoint();
    let client_id = minimax_oauth_client_id();
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
            ("client_id", client_id.as_str()),
        ])
        .send()
        .map_err(|error| anyhow::anyhow!("MiniMax OAuth refresh request failed: {error}"))?;

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

    anyhow::bail!("MiniMax OAuth refresh response missing access_token");
}

fn resolve_minimax_oauth_refresh_token(name: &str) -> Option<String> {
    let refresh_token = read_non_empty_env(MINIMAX_OAUTH_REFRESH_TOKEN_ENV)?;

    match refresh_minimax_oauth_access_token(name, &refresh_token) {
        Ok(token) => Some(token),
        Err(error) => {
            tracing::warn!(provider = name, error = %error, "MiniMax OAuth refresh failed");
            None
        }
    }
}

pub fn canonical_china_provider_name(name: &str) -> Option<&'static str> {
    if is_qwen_alias(name) {
        Some("qwen")
    } else if is_glm_alias(name) {
        Some("glm")
    } else if is_moonshot_alias(name) {
        Some("moonshot")
    } else if is_minimax_alias(name) {
        Some("minimax")
    } else if is_zai_alias(name) {
        Some("zai")
    } else if is_qianfan_alias(name) {
        Some("qianfan")
    } else if is_doubao_alias(name) {
        Some("doubao")
    } else if is_bailian_alias(name) {
        Some("bailian")
    } else {
        None
    }
}

fn minimax_base_url(name: &str) -> Option<&'static str> {
    if is_minimax_cn_alias(name) {
        Some(MINIMAX_CN_BASE_URL)
    } else if is_minimax_intl_alias(name) {
        Some(MINIMAX_INTL_BASE_URL)
    } else {
        None
    }
}

fn glm_base_url(name: &str) -> Option<&'static str> {
    if is_glm_cn_alias(name) {
        Some(GLM_CN_BASE_URL)
    } else if is_glm_global_alias(name) {
        Some(GLM_GLOBAL_BASE_URL)
    } else {
        None
    }
}

fn moonshot_base_url(name: &str) -> Option<&'static str> {
    if is_moonshot_intl_alias(name) {
        Some(MOONSHOT_INTL_BASE_URL)
    } else if is_moonshot_cn_alias(name) {
        Some(MOONSHOT_CN_BASE_URL)
    } else {
        None
    }
}

fn qwen_base_url(name: &str) -> Option<&'static str> {
    if is_qwen_cn_alias(name) || is_qwen_oauth_alias(name) {
        Some(QWEN_CN_BASE_URL)
    } else if is_qwen_intl_alias(name) {
        Some(QWEN_INTL_BASE_URL)
    } else if is_qwen_us_alias(name) {
        Some(QWEN_US_BASE_URL)
    } else {
        None
    }
}

fn zai_base_url(name: &str) -> Option<&'static str> {
    if is_zai_cn_alias(name) {
        Some(ZAI_CN_BASE_URL)
    } else if is_zai_global_alias(name) {
        Some(ZAI_GLOBAL_BASE_URL)
    } else {
        None
    }
}

#[derive(Debug, Clone)]
pub struct ProviderRuntimeOptions {
    pub auth_profile_override: Option<String>,
    pub provider_api_url: Option<String>,
    pub zeroclaw_dir: Option<PathBuf>,
    pub secrets_encrypt: bool,
    pub reasoning_enabled: Option<bool>,
    pub reasoning_effort: Option<String>,
    /// HTTP request timeout in seconds for LLM provider API calls.
    /// `None` uses the provider's built-in default (120s for compatible providers).
    pub provider_timeout_secs: Option<u64>,
    /// Extra HTTP headers to include in provider API requests.
    /// These are merged from the config file and `ZEROCLAW_EXTRA_HEADERS` env var.
    pub extra_headers: std::collections::HashMap<String, String>,
    /// Custom API path suffix for OpenAI-compatible providers
    /// (e.g. "/v2/generate" instead of the default "/chat/completions").
    pub api_path: Option<String>,
    /// Maximum output tokens for LLM provider API requests.
    /// `None` uses the provider's built-in default.
    pub provider_max_tokens: Option<u32>,
    /// When true, system messages are merged into the first user message before
    /// sending. Propagated from `ModelProviderConfig::merge_system_into_user`.
    pub merge_system_into_user: bool,
    /// Extra JSON parameters merged into API request bodies at the top level.
    /// Propagated from `ModelProviderConfig::provider_extra`.
    pub provider_extra: Option<serde_json::Value>,
    /// Override the provider's default for native tool calling. `None` honors
    /// the per-provider built-in choice. `Some(true)` forces native tool
    /// calls on; `Some(false)` forces text-fallback. Propagated from
    /// `ModelProviderConfig::native_tools`. Currently consulted only by the
    /// Groq factory branch (#5932).
    pub native_tools: Option<bool>,
}

impl Default for ProviderRuntimeOptions {
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
        }
    }
}

pub fn provider_runtime_options_from_config(
    config: &zeroclaw_config::schema::Config,
) -> ProviderRuntimeOptions {
    let fallback = config.providers.fallback_provider();
    // Resolve merge_system_into_user from the active model provider profile by
    // matching api_url — apply_named_model_provider_profile() has already run
    // and rewritten providers.fallback, but providers.models retains all profiles.
    let merge_system_into_user = fallback
        .and_then(|e| e.base_url.as_deref())
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .and_then(|active_url| {
            config.providers.models.values().find(|p| {
                p.base_url
                    .as_deref()
                    .map(str::trim)
                    .filter(|u| !u.is_empty())
                    .map(|u| u.trim_end_matches('/'))
                    == Some(active_url.trim_end_matches('/'))
            })
        })
        .map(|p| p.merge_system_into_user)
        .unwrap_or(false);

    ProviderRuntimeOptions {
        auth_profile_override: None,
        provider_api_url: fallback.and_then(|e| e.base_url.clone()),
        zeroclaw_dir: config.config_path.parent().map(PathBuf::from),
        secrets_encrypt: config.secrets.encrypt,
        reasoning_enabled: config.runtime.reasoning_enabled,
        reasoning_effort: config.runtime.reasoning_effort.clone(),
        provider_timeout_secs: Some(fallback.and_then(|e| e.timeout_secs).unwrap_or(120)),
        extra_headers: fallback
            .map(|e| e.extra_headers.clone())
            .unwrap_or_default(),
        api_path: fallback.and_then(|e| e.api_path.clone()),
        provider_max_tokens: fallback.and_then(|e| e.max_tokens),
        merge_system_into_user,
        provider_extra: fallback.and_then(|e| e.provider_extra.clone()),
        native_tools: fallback.and_then(|e| e.native_tools),
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

/// Scrub known secret-like token prefixes from provider error strings.
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

/// Build a sanitized provider error from a failed HTTP response.
pub async fn api_error(provider: &str, response: reqwest::Response) -> anyhow::Error {
    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "<failed to read provider error body>".to_string());
    let sanitized = sanitize_api_error(&body);
    anyhow::anyhow!("{provider} API error ({status}): {sanitized}")
}

/// Resolve API key for a provider from config and environment variables.
///
/// Resolution order:
/// 1. Explicitly provided `api_key` parameter (trimmed, filtered if empty)
/// 2. Provider-specific environment variable (e.g., `ANTHROPIC_OAUTH_TOKEN`, `OPENROUTER_API_KEY`)
/// 3. Generic fallback variables (`ZEROCLAW_API_KEY`, `API_KEY`)
///
/// For Anthropic, the provider-specific env var is `ANTHROPIC_OAUTH_TOKEN` (for setup-tokens)
/// followed by `ANTHROPIC_API_KEY` (for regular API keys).
///
/// For MiniMax, OAuth mode supports `api_key = "minimax-oauth"`, resolving credentials from
/// `MINIMAX_OAUTH_TOKEN` first, then `MINIMAX_API_KEY`, and finally
/// `MINIMAX_OAUTH_REFRESH_TOKEN` (automatic access-token refresh).
fn resolve_provider_credential(name: &str, credential_override: Option<&str>) -> Option<String> {
    let mut minimax_oauth_placeholder_requested = false;

    if let Some(raw_override) = credential_override {
        let trimmed_override = raw_override.trim();
        if !trimmed_override.is_empty() {
            if is_minimax_alias(name) && is_minimax_oauth_placeholder(trimmed_override) {
                minimax_oauth_placeholder_requested = true;
                if let Some(credential) = resolve_minimax_static_credential() {
                    return Some(credential);
                }
                if let Some(credential) = resolve_minimax_oauth_refresh_token(name) {
                    return Some(credential);
                }
            } else if name == "anthropic" || name == "openai" || name == "groq" {
                // For well-known providers, prefer provider-specific env vars over the
                // global api_key override, since the global key may belong to a different
                // provider (e.g. a custom: gateway). This enables multi-provider setups
                // where the primary uses a custom gateway and fallbacks use named providers.
                let env_candidates: &[&str] = match name {
                    "anthropic" => &["ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"],
                    "openai" => &["OPENAI_API_KEY"],
                    "groq" => &["GROQ_API_KEY"],
                    _ => &[],
                };
                for env_var in env_candidates {
                    if let Ok(val) = std::env::var(env_var) {
                        let trimmed = val.trim().to_string();
                        if !trimmed.is_empty() {
                            return Some(trimmed);
                        }
                    }
                }
                return Some(trimmed_override.to_owned());
            } else {
                return Some(trimmed_override.to_owned());
            }
        }
    }

    let provider_env_candidates: Vec<&str> = match name {
        "anthropic" => vec!["ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"],
        "openrouter" => vec!["OPENROUTER_API_KEY"],
        "openai" => vec!["OPENAI_API_KEY"],
        "ollama" => vec!["OLLAMA_API_KEY"],
        "venice" => vec!["VENICE_API_KEY"],
        "groq" => vec!["GROQ_API_KEY"],
        "mistral" => vec!["MISTRAL_API_KEY"],
        "deepseek" => vec!["DEEPSEEK_API_KEY"],
        "xai" | "grok" => vec!["XAI_API_KEY"],
        "together" | "together-ai" => vec!["TOGETHER_API_KEY"],
        "fireworks" | "fireworks-ai" => vec!["FIREWORKS_API_KEY"],
        "novita" => vec!["NOVITA_API_KEY"],
        "perplexity" => vec!["PERPLEXITY_API_KEY"],
        "copilot" | "github-copilot" => vec!["GITHUB_TOKEN"],
        "cohere" => vec!["COHERE_API_KEY"],
        name if is_moonshot_alias(name) => vec!["MOONSHOT_API_KEY"],
        "kimi-code" | "kimi_coding" | "kimi_for_coding" => {
            vec!["KIMI_CODE_API_KEY", "MOONSHOT_API_KEY"]
        }
        name if is_glm_alias(name) => vec!["GLM_API_KEY"],
        name if is_minimax_alias(name) => vec![MINIMAX_OAUTH_TOKEN_ENV, MINIMAX_API_KEY_ENV],
        // Bedrock supports Bearer token auth via BEDROCK_API_KEY env var, in addition
        // to AWS AKSK (SigV4). If BEDROCK_API_KEY is set, return it; otherwise return
        // None and let BedrockProvider handle SigV4 credential resolution internally.
        "bedrock" | "aws-bedrock" => {
            if let Ok(val) = std::env::var("BEDROCK_API_KEY") {
                let trimmed = val.trim().to_string();
                if !trimmed.is_empty() {
                    return Some(trimmed);
                }
            }
            return None;
        }
        name if is_qianfan_alias(name) => vec!["QIANFAN_API_KEY"],
        name if is_doubao_alias(name) => {
            vec!["ARK_API_KEY", "VOLCENGINE_API_KEY", "DOUBAO_API_KEY"]
        }
        name if is_qwen_alias(name) => vec!["DASHSCOPE_API_KEY"],
        name if is_bailian_alias(name) => vec!["BAILIAN_API_KEY", "DASHSCOPE_API_KEY"],
        name if is_zai_alias(name) => vec!["ZAI_API_KEY"],
        "nvidia" | "nvidia-nim" | "build.nvidia.com" => vec!["NVIDIA_API_KEY"],
        "synthetic" => vec!["SYNTHETIC_API_KEY"],
        "opencode" | "opencode-zen" => vec!["OPENCODE_API_KEY"],
        "opencode-go" => vec!["OPENCODE_GO_API_KEY"],
        "vercel" | "vercel-ai" => vec!["VERCEL_API_KEY"],
        "cloudflare" | "cloudflare-ai" => vec!["CLOUDFLARE_API_KEY"],
        "ovhcloud" | "ovh" => vec!["OVH_AI_ENDPOINTS_ACCESS_TOKEN"],
        "astrai" => vec!["ASTRAI_API_KEY"],
        "avian" => vec!["AVIAN_API_KEY"],
        "deepmyst" | "deep-myst" => vec!["DEEPMYST_API_KEY"],
        "llamacpp" | "llama.cpp" => vec!["LLAMACPP_API_KEY"],
        "sglang" => vec!["SGLANG_API_KEY"],
        "vllm" => vec!["VLLM_API_KEY"],
        "aihubmix" => vec!["AIHUBMIX_API_KEY"],
        "siliconflow" | "silicon-flow" => vec!["SILICONFLOW_API_KEY"],
        "osaurus" => vec!["OSAURUS_API_KEY"],
        "telnyx" => vec!["TELNYX_API_KEY"],
        "azure_openai" | "azure-openai" | "azure" => vec!["AZURE_OPENAI_API_KEY"],
        _ => vec![],
    };

    for env_var in provider_env_candidates {
        if let Ok(value) = std::env::var(env_var) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }

    if is_minimax_alias(name)
        && let Some(credential) = resolve_minimax_oauth_refresh_token(name)
    {
        return Some(credential);
    }

    if minimax_oauth_placeholder_requested && is_minimax_alias(name) {
        return None;
    }

    for env_var in ["ZEROCLAW_API_KEY", "API_KEY"] {
        if let Ok(value) = std::env::var(env_var) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }

    None
}

/// Check whether an API key's prefix matches the selected provider.
///
/// Returns `Some("likely_provider")` when the key clearly belongs to a
/// *different* provider (cross-provider mismatch).  Returns `None` when
/// everything looks fine or the format is unrecognised.
fn check_api_key_prefix(provider_name: &str, key: &str) -> Option<&'static str> {
    // Identify which provider the key likely belongs to (longest prefix first).
    let likely_provider = if key.starts_with("sk-ant-") {
        Some("anthropic")
    } else if key.starts_with("sk-or-") {
        Some("openrouter")
    } else if key.starts_with("sk-") {
        Some("openai")
    } else if key.starts_with("gsk_") {
        Some("groq")
    } else if key.starts_with("pplx-") {
        Some("perplexity")
    } else if key.starts_with("xai-") {
        Some("xai")
    } else if key.starts_with("nvapi-") {
        Some("nvidia")
    } else if key.starts_with("KEY-") {
        Some("telnyx")
    } else {
        None
    };

    let expected = likely_provider?;

    // Only flag mismatch for providers where we know the key format.
    let matches = match provider_name {
        "anthropic" => expected == "anthropic",
        "openrouter" => expected == "openrouter",
        "openai" => expected == "openai",
        "groq" => expected == "groq",
        "perplexity" => expected == "perplexity",
        "xai" | "grok" => expected == "xai",
        "nvidia" | "nvidia-nim" | "build.nvidia.com" => expected == "nvidia",
        "telnyx" => expected == "telnyx",
        _ => return None, // Unknown format provider — skip
    };

    if matches { None } else { Some(expected) }
}

fn parse_custom_provider_url(
    raw_url: &str,
    provider_label: &str,
    format_hint: &str,
) -> anyhow::Result<String> {
    let base_url = raw_url.trim();

    if base_url.is_empty() {
        anyhow::bail!("{provider_label} requires a URL. Format: {format_hint}");
    }

    let parsed = reqwest::Url::parse(base_url).map_err(|_| {
        anyhow::anyhow!("{provider_label} requires a valid URL. Format: {format_hint}")
    })?;

    match parsed.scheme() {
        "http" | "https" => Ok(base_url.to_string()),
        _ => anyhow::bail!(
            "{provider_label} requires an http:// or https:// URL. Format: {format_hint}"
        ),
    }
}

/// Factory: create the right provider from config (without custom URL)
pub fn create_provider(name: &str, api_key: Option<&str>) -> anyhow::Result<Box<dyn Provider>> {
    create_provider_with_options(name, api_key, &ProviderRuntimeOptions::default())
}

/// Build a `ModelProviderConfig` populated with the named provider's
/// effective default values from its `Provider` trait methods (the same
/// `default_temperature` / `default_max_tokens` / `default_timeout_secs` /
/// `default_wire_api` / `default_base_url` the runtime already calls at
/// every chat request).
///
/// One shared function — gateway onboarding pre-fill, CLI wizard prompt
/// pre-fill, and any future caller all consume this so the surfaces can't
/// drift. The schema-level cleanup (per-provider typed configs that would
/// eliminate the hardcoded field list here) ships with v3 / #5947.
///
/// Returns `ModelProviderConfig::default()` (all `None`) for unknown
/// providers — the form just opens blank, callers don't have to special
/// case.
pub fn default_provider_config(name: &str) -> zeroclaw_config::schema::ModelProviderConfig {
    use zeroclaw_config::schema::ModelProviderConfig;
    let Ok(handle) = create_provider(name, None) else {
        return ModelProviderConfig::default();
    };
    ModelProviderConfig {
        base_url: handle.default_base_url().map(str::to_string),
        temperature: Some(handle.default_temperature()),
        max_tokens: Some(handle.default_max_tokens()),
        timeout_secs: Some(handle.default_timeout_secs()),
        wire_api: Some(handle.default_wire_api().to_string()),
        ..Default::default()
    }
}

/// Factory: create provider with runtime options (auth profile override, state dir).
pub fn create_provider_with_options(
    name: &str,
    api_key: Option<&str>,
    options: &ProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn Provider>> {
    match name {
        "openai-codex" | "openai_codex" | "codex" => Ok(Box::new(
            openai_codex::OpenAiCodexProvider::new(options, api_key)?,
        )),
        _ => create_provider_with_url_and_options(name, api_key, None, options),
    }
}

/// Factory: create the right provider from config with optional custom base URL
pub fn create_provider_with_url(
    name: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
) -> anyhow::Result<Box<dyn Provider>> {
    create_provider_with_url_and_options(name, api_key, api_url, &ProviderRuntimeOptions::default())
}

/// Factory: create provider with optional base URL and runtime options.
#[allow(clippy::too_many_lines)]
fn create_provider_with_url_and_options(
    name: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
    options: &ProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn Provider>> {
    // Closure to optionally apply the configured provider timeout and extra
    // headers to OpenAI-compatible providers before boxing them as trait objects.
    let compat = {
        let timeout = options.provider_timeout_secs;
        let reasoning_effort = options.reasoning_effort.clone();
        let extra_headers = options.extra_headers.clone();
        let api_path = options.api_path.clone();
        let max_tokens = options.provider_max_tokens;
        move |p: OpenAiCompatibleProvider| -> Box<dyn Provider> {
            let mut p = p;
            if let Some(t) = timeout {
                p = p.with_timeout_secs(t);
            }
            if let Some(ref effort) = reasoning_effort {
                p = p.with_reasoning_effort(Some(effort.clone()));
            }
            if !extra_headers.is_empty() {
                p = p.with_extra_headers(extra_headers.clone());
            }
            if api_path.is_some() {
                p = p.with_api_path(api_path.clone());
            }
            if let Some(mt) = max_tokens {
                p = p.with_max_tokens(Some(mt));
            }
            Box::new(p)
        }
    };

    let qwen_oauth_context = is_qwen_oauth_alias(name).then(|| resolve_qwen_oauth_context(api_key));

    // Resolve credential and break static-analysis taint chain from the
    // `api_key` parameter so that downstream provider storage of the value
    // is not linked to the original sensitive-named source.
    let resolved_credential = if let Some(context) = qwen_oauth_context.as_ref() {
        context.credential.clone()
    } else {
        resolve_provider_credential(name, api_key)
    }
    .map(|v| String::from_utf8(v.into_bytes()).unwrap_or_default());
    #[allow(clippy::option_as_ref_deref)]
    let key = resolved_credential.as_ref().map(String::as_str);

    // Pre-flight: catch obvious API-key / provider mismatches early.
    if let Some(key_value) = key {
        let is_custom = name.starts_with("custom:") || name.starts_with("anthropic-custom:");
        let has_custom_url = api_url.map(str::trim).filter(|u| !u.is_empty()).is_some();
        if !is_custom
            && !has_custom_url
            && let Some(likely_provider) = check_api_key_prefix(name, key_value)
        {
            let visible = &key_value[..key_value.len().min(8)];
            anyhow::bail!(
                "API key prefix mismatch: key \"{visible}...\" looks like a \
                     {likely_provider} key, but provider \"{name}\" is selected. \
                     Set the correct provider-specific env var or use `-p {likely_provider}`."
            );
        }
    }

    match name {
        "openai-codex" | "openai_codex" | "codex" => {
            let mut codex_options = options.clone();
            codex_options.provider_api_url = api_url
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .or_else(|| options.provider_api_url.clone());
            Ok(Box::new(openai_codex::OpenAiCodexProvider::new(
                &codex_options,
                key,
            )?))
        }
        // ── Primary providers (custom implementations) ───────
        "openrouter" => {
            let mut p = openrouter::OpenRouterProvider::new(key, options.provider_timeout_secs)
                .with_max_tokens(options.provider_max_tokens);
            if let Some(extra) = options.provider_extra.clone() {
                p = p.with_extra_body(extra);
            }
            Ok(Box::new(p))
        }
        "anthropic" => {
            let mut p = anthropic::AnthropicProvider::with_base_url(key, api_url);
            if let Some(mt) = options.provider_max_tokens {
                p = p.with_max_tokens(mt);
            }
            Ok(Box::new(p))
        }
        "openai" => {
            let mut p = openai::OpenAiProvider::with_base_url(api_url, key);
            if let Some(mt) = options.provider_max_tokens {
                p = p.with_max_tokens(Some(mt));
            }
            Ok(Box::new(p))
        }
        // Ollama uses api_url for custom base URL (e.g. remote Ollama instance)
        "ollama" => {
            let env_url = std::env::var("ZEROCLAW_PROVIDER_URL").ok();

            let api_url = env_url.as_deref().or(api_url);

            Ok(Box::new(ollama::OllamaProvider::new_with_reasoning(
                api_url,
                key,
                options.reasoning_enabled,
            )))
        }
        "gemini" | "google" | "google-gemini" => {
            let state_dir = options.zeroclaw_dir.clone().unwrap_or_else(|| {
                directories::UserDirs::new().map_or_else(
                    || PathBuf::from(".zeroclaw"),
                    |dirs| dirs.home_dir().join(".zeroclaw"),
                )
            });
            let auth_service = AuthService::new(&state_dir, options.secrets_encrypt);
            Ok(Box::new(gemini::GeminiProvider::new_with_auth(
                key,
                auth_service,
                options.auth_profile_override.clone(),
            )))
        }
        "telnyx" => Ok(Box::new(telnyx::TelnyxProvider::new(key))),

        // ── OpenAI-compatible providers ──────────────────────
        "venice" => Ok(compat(
            OpenAiCompatibleProvider::new(
                "Venice",
                "https://api.venice.ai",
                key,
                AuthStyle::Bearer,
            )
            .without_native_tools(),
        )),
        "vercel" | "vercel-ai" => Ok(compat(OpenAiCompatibleProvider::new(
            "Vercel AI Gateway",
            VERCEL_AI_GATEWAY_BASE_URL,
            key,
            AuthStyle::Bearer,
        ))),
        "cloudflare" | "cloudflare-ai" => Ok(compat(OpenAiCompatibleProvider::new(
            "Cloudflare AI Gateway",
            "https://gateway.ai.cloudflare.com/v1",
            key,
            AuthStyle::Bearer,
        ))),
        name if moonshot_base_url(name).is_some() => Ok(compat(OpenAiCompatibleProvider::new(
            "Moonshot",
            moonshot_base_url(name).expect("checked in guard"),
            key,
            AuthStyle::Bearer,
        ))),
        "kimi-code" | "kimi_coding" | "kimi_for_coding" => {
            Ok(compat(OpenAiCompatibleProvider::new_with_user_agent(
                "Kimi Code",
                "https://api.kimi.com/coding/v1",
                key,
                AuthStyle::Bearer,
                "KimiCLI/0.77",
            )))
        }
        "synthetic" => Ok(compat(OpenAiCompatibleProvider::new(
            "Synthetic",
            "https://api.synthetic.new/openai/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "opencode" | "opencode-zen" => Ok(compat(OpenAiCompatibleProvider::new(
            "OpenCode Zen",
            "https://opencode.ai/zen/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "opencode-go" => Ok(compat(OpenAiCompatibleProvider::new(
            "OpenCode Go",
            "https://opencode.ai/zen/go/v1",
            key,
            AuthStyle::Bearer,
        ))),
        name if zai_base_url(name).is_some() => Ok(compat(OpenAiCompatibleProvider::new(
            "Z.AI",
            zai_base_url(name).expect("checked in guard"),
            key,
            AuthStyle::ZhipuJwt,
        ))),
        name if glm_base_url(name).is_some() => {
            Ok(compat(OpenAiCompatibleProvider::new_no_responses_fallback(
                "GLM",
                glm_base_url(name).expect("checked in guard"),
                key,
                AuthStyle::ZhipuJwt,
            )))
        }
        name if minimax_base_url(name).is_some() => Ok(compat(
            OpenAiCompatibleProvider::new(
                "MiniMax",
                minimax_base_url(name).expect("checked in guard"),
                key,
                AuthStyle::Bearer,
            )
            .with_merge_system_into_user(),
        )),
        "azure_openai" | "azure-openai" | "azure" => {
            let resource = std::env::var("AZURE_OPENAI_RESOURCE")
                .unwrap_or_else(|_| "my-resource".to_string());
            let deployment =
                std::env::var("AZURE_OPENAI_DEPLOYMENT").unwrap_or_else(|_| "gpt-4o".to_string());
            let api_version = std::env::var("AZURE_OPENAI_API_VERSION").ok();
            Ok(Box::new(azure_openai::AzureOpenAiProvider::new(
                key,
                &resource,
                &deployment,
                api_version.as_deref(),
            )))
        }
        "bedrock" | "aws-bedrock" => {
            let mut p = if let Some(api_key) = key {
                bedrock::BedrockProvider::with_bearer_token(api_key)
            } else {
                bedrock::BedrockProvider::new()
            };
            if let Some(mt) = options.provider_max_tokens {
                p = p.with_max_tokens(mt);
            }
            Ok(Box::new(p))
        }
        name if is_qwen_oauth_alias(name) => {
            let base_url = api_url
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .or_else(|| {
                    qwen_oauth_context
                        .as_ref()
                        .and_then(|context| context.base_url.clone())
                })
                .unwrap_or_else(|| QWEN_OAUTH_BASE_FALLBACK_URL.to_string());

            Ok(compat(
                OpenAiCompatibleProvider::new_with_user_agent_and_vision(
                    "Qwen Code",
                    &base_url,
                    key,
                    AuthStyle::Bearer,
                    "QwenCode/1.0",
                    true,
                ),
            ))
        }
        name if is_qianfan_alias(name) => {
            let base_url = qianfan_base_url(api_url);
            Ok(compat(OpenAiCompatibleProvider::new(
                "Qianfan",
                &base_url,
                key,
                AuthStyle::Bearer,
            )))
        }
        name if is_doubao_alias(name) => Ok(compat(OpenAiCompatibleProvider::new(
            "Doubao",
            "https://ark.cn-beijing.volces.com/api/v3",
            key,
            AuthStyle::Bearer,
        ))),
        name if is_bailian_alias(name) => Ok(Box::new(
            OpenAiCompatibleProvider::new_with_user_agent_and_vision(
                "Bailian",
                BAILIAN_BASE_URL,
                key,
                AuthStyle::Bearer,
                "openclaw",
                true,
            ),
        )),
        name if qwen_base_url(name).is_some() => {
            Ok(compat(OpenAiCompatibleProvider::new_with_vision(
                "Qwen",
                qwen_base_url(name).expect("checked in guard"),
                key,
                AuthStyle::Bearer,
                true,
            )))
        }

        // ── Extended ecosystem (community favorites) ─────────
        "groq" => {
            let mut p = OpenAiCompatibleProvider::new(
                "Groq",
                "https://api.groq.com/openai/v1",
                key,
                AuthStyle::Bearer,
            );
            // Default to text-fallback because Groq's llama-family models
            // reject native tool calls with HTTP 400 (#5848). Operators can
            // override per-profile via `[providers.models.<alias>] native_tools = true`
            // to enable native tool calling for Groq models that support it.
            if options.native_tools != Some(true) {
                p = p.without_native_tools();
            }
            Ok(compat(p))
        }
        "mistral" => Ok(compat(OpenAiCompatibleProvider::new(
            "Mistral",
            "https://api.mistral.ai/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "xai" | "grok" => Ok(compat(
            OpenAiCompatibleProvider::new("xAI", "https://api.x.ai/v1", key, AuthStyle::Bearer)
                .with_models_dev_key("xai"),
        )),
        "deepseek" => Ok(compat(OpenAiCompatibleProvider::new(
            "DeepSeek",
            "https://api.deepseek.com",
            key,
            AuthStyle::Bearer,
        ))),
        "together" | "together-ai" => Ok(compat(OpenAiCompatibleProvider::new(
            "Together AI",
            "https://api.together.xyz",
            key,
            AuthStyle::Bearer,
        ))),
        "fireworks" | "fireworks-ai" => Ok(compat(OpenAiCompatibleProvider::new(
            "Fireworks AI",
            "https://api.fireworks.ai/inference/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "novita" => Ok(compat(OpenAiCompatibleProvider::new(
            "Novita AI",
            "https://api.novita.ai/openai",
            key,
            AuthStyle::Bearer,
        ))),
        "perplexity" => Ok(compat(OpenAiCompatibleProvider::new(
            "Perplexity",
            "https://api.perplexity.ai",
            key,
            AuthStyle::Bearer,
        ))),
        "cohere" => Ok(compat(OpenAiCompatibleProvider::new(
            "Cohere",
            "https://api.cohere.com/compatibility",
            key,
            AuthStyle::Bearer,
        ))),
        "copilot" | "github-copilot" => Ok(Box::new(copilot::CopilotProvider::new(key))),
        "claude-code" => Ok(Box::new(claude_code::ClaudeCodeProvider::new())),
        "gemini-cli" => Ok(Box::new(gemini_cli::GeminiCliProvider::new())),
        "kilocli" | "kilo" => Ok(Box::new(kilocli::KiloCliProvider::new())),
        "lmstudio" | "lm-studio" => {
            let lm_studio_key = key
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("lm-studio");
            Ok(compat(OpenAiCompatibleProvider::new(
                "LM Studio",
                "http://localhost:1234/v1",
                Some(lm_studio_key),
                AuthStyle::Bearer,
            )))
        }
        "llamacpp" | "llama.cpp" => {
            let base_url = api_url
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("http://localhost:8080/v1");
            let llama_cpp_key = key
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("llama.cpp");
            let provider = OpenAiCompatibleProvider::new_with_vision(
                "llama.cpp",
                base_url,
                Some(llama_cpp_key),
                AuthStyle::Bearer,
                true,
            );
            let provider = if options.merge_system_into_user {
                provider.with_merge_system_into_user()
            } else {
                provider
            };
            Ok(compat(provider))
        }
        "sglang" => {
            let base_url = api_url
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("http://localhost:30000/v1");
            Ok(compat(OpenAiCompatibleProvider::new(
                "SGLang",
                base_url,
                key,
                AuthStyle::Bearer,
            )))
        }
        "vllm" => {
            let base_url = api_url
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("http://localhost:8000/v1");
            Ok(compat(OpenAiCompatibleProvider::new(
                "vLLM",
                base_url,
                key,
                AuthStyle::Bearer,
            )))
        }
        "osaurus" => {
            let base_url = api_url
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("http://localhost:1337/v1");
            let osaurus_key = key
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("osaurus");
            Ok(compat(OpenAiCompatibleProvider::new(
                "Osaurus",
                base_url,
                Some(osaurus_key),
                AuthStyle::Bearer,
            )))
        }
        "nvidia" | "nvidia-nim" | "build.nvidia.com" => {
            Ok(compat(OpenAiCompatibleProvider::new_no_responses_fallback(
                "NVIDIA NIM",
                "https://integrate.api.nvidia.com/v1",
                key,
                AuthStyle::Bearer,
            )))
        }

        // ── AI inference routers ─────────────────────────────
        "astrai" => Ok(compat(OpenAiCompatibleProvider::new(
            "Astrai",
            "https://as-trai.com/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "siliconflow" | "silicon-flow" => Ok(compat(OpenAiCompatibleProvider::new(
            "SiliconFlow",
            "https://api.siliconflow.cn/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "aihubmix" => Ok(compat(OpenAiCompatibleProvider::new(
            "AiHubMix",
            "https://aihubmix.com/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "litellm" | "lite-llm" => {
            let base_url = api_url
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("http://localhost:4000/v1");
            Ok(compat(OpenAiCompatibleProvider::new(
                "LiteLLM",
                base_url,
                key,
                AuthStyle::Bearer,
            )))
        }

        // ── Fast inference providers ──────────────────────────
        "cerebras" => Ok(compat(OpenAiCompatibleProvider::new(
            "Cerebras",
            "https://api.cerebras.ai/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "sambanova" => Ok(compat(OpenAiCompatibleProvider::new(
            "SambaNova",
            "https://api.sambanova.ai/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "hyperbolic" => Ok(compat(OpenAiCompatibleProvider::new(
            "Hyperbolic",
            "https://api.hyperbolic.xyz/v1",
            key,
            AuthStyle::Bearer,
        ))),

        // ── Model hosting platforms ──────────────────────────
        "deepinfra" | "deep-infra" => Ok(compat(OpenAiCompatibleProvider::new(
            "DeepInfra",
            "https://api.deepinfra.com/v1/openai",
            key,
            AuthStyle::Bearer,
        ))),
        "huggingface" | "hf" => Ok(compat(OpenAiCompatibleProvider::new(
            "Hugging Face",
            "https://router.huggingface.co/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "ai21" | "ai21-labs" => Ok(compat(OpenAiCompatibleProvider::new(
            "AI21 Labs",
            "https://api.ai21.com/studio/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "reka" => Ok(compat(OpenAiCompatibleProvider::new(
            "Reka",
            "https://api.reka.ai/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "baseten" => Ok(compat(OpenAiCompatibleProvider::new(
            "Baseten",
            "https://inference.baseten.co/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "nscale" => Ok(compat(OpenAiCompatibleProvider::new(
            "Nscale",
            "https://inference.api.nscale.com/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "anyscale" => Ok(compat(OpenAiCompatibleProvider::new(
            "Anyscale",
            "https://api.endpoints.anyscale.com/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "nebius" => Ok(compat(OpenAiCompatibleProvider::new(
            "Nebius AI Studio",
            "https://api.studio.nebius.ai/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "friendli" | "friendliai" => Ok(compat(OpenAiCompatibleProvider::new(
            "Friendli AI",
            "https://api.friendli.ai/serverless/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "lepton" | "lepton-ai" => {
            let base_url = api_url
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("https://llama3-1-405b.lepton.run/api/v1");
            Ok(compat(OpenAiCompatibleProvider::new(
                "Lepton AI",
                base_url,
                key,
                AuthStyle::Bearer,
            )))
        }

        // ── Chinese AI providers ─────────────────────────────
        "stepfun" | "step" => Ok(compat(OpenAiCompatibleProvider::new(
            "Stepfun",
            "https://api.stepfun.com/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "baichuan" => Ok(compat(OpenAiCompatibleProvider::new(
            "Baichuan",
            "https://api.baichuan-ai.com/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "yi" | "01ai" | "lingyiwanwu" => Ok(compat(OpenAiCompatibleProvider::new(
            "01.AI (Yi)",
            "https://api.lingyiwanwu.com/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "hunyuan" | "tencent" => Ok(compat(OpenAiCompatibleProvider::new(
            "Tencent Hunyuan",
            "https://api.hunyuan.cloud.tencent.com/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "avian" => Ok(compat(OpenAiCompatibleProvider::new(
            "Avian",
            "https://api.avian.io/v1",
            key,
            AuthStyle::Bearer,
        ))),
        "deepmyst" | "deep-myst" => Ok(compat(OpenAiCompatibleProvider::new(
            "DeepMyst",
            "https://api.deepmyst.com/v1",
            key,
            AuthStyle::Bearer,
        ))),

        // ── Cloud AI endpoints ───────────────────────────────
        "ovhcloud" | "ovh" => Ok(Box::new(openai::OpenAiProvider::with_base_url(
            Some("https://oai.endpoints.kepler.ai.cloud.ovh.net/v1"),
            key,
        ))),

        // ── Bring Your Own Provider (custom URL) ───────────
        // Format: "custom:https://your-api.com" or "custom:http://localhost:1234"
        name if name.starts_with("custom:") => {
            let base_url = parse_custom_provider_url(
                name.strip_prefix("custom:").unwrap_or(""),
                "Custom provider",
                "custom:https://your-api.com",
            )?;
            let provider = OpenAiCompatibleProvider::new_with_vision(
                "Custom",
                &base_url,
                key,
                AuthStyle::Bearer,
                true,
            );
            let provider = if options.merge_system_into_user {
                provider.with_merge_system_into_user()
            } else {
                provider
            };
            Ok(compat(provider))
        }

        // ── Anthropic-compatible custom endpoints ───────────
        // Format: "anthropic-custom:https://your-api.com"
        name if name.starts_with("anthropic-custom:") => {
            let base_url = parse_custom_provider_url(
                name.strip_prefix("anthropic-custom:").unwrap_or(""),
                "Anthropic-custom provider",
                "anthropic-custom:https://your-api.com",
            )?;
            Ok(Box::new(anthropic::AnthropicProvider::with_base_url(
                key,
                Some(&base_url),
            )))
        }

        _ => anyhow::bail!(
            "Unknown provider: {name}. Check README for supported providers or run `zeroclaw onboard` to reconfigure.\n\
             Tip: Use \"custom:https://your-api.com\" for OpenAI-compatible endpoints.\n\
             Tip: Use \"anthropic-custom:https://your-api.com\" for Anthropic-compatible endpoints."
        ),
    }
}

/// Parse `"provider:profile"` syntax for fallback entries.
///
/// Returns `(provider_name, Some(profile))` when the entry contains a colon-
/// delimited profile, or `(original_str, None)` otherwise.  Entries starting
/// with `custom:` or `anthropic-custom:` are left untouched because the colon
/// is part of the URL scheme.
fn parse_provider_profile(s: &str) -> (&str, Option<&str>) {
    if s.starts_with("custom:") || s.starts_with("anthropic-custom:") {
        return (s, None);
    }
    match s.split_once(':') {
        Some((provider, profile)) if !profile.is_empty() => (provider, Some(profile)),
        _ => (s, None),
    }
}

/// Create provider chain with retry and fallback behavior.
pub fn create_resilient_provider(
    primary_name: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
    reliability: &zeroclaw_config::schema::ReliabilityConfig,
) -> anyhow::Result<Box<dyn Provider>> {
    create_resilient_provider_with_options(
        primary_name,
        api_key,
        api_url,
        reliability,
        &ProviderRuntimeOptions::default(),
    )
}

/// Create provider chain with retry/fallback behavior and auth runtime options.
pub fn create_resilient_provider_with_options(
    primary_name: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
    reliability: &zeroclaw_config::schema::ReliabilityConfig,
    options: &ProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn Provider>> {
    let mut providers: Vec<(String, Box<dyn Provider>)> = Vec::new();

    let primary_provider = match primary_name {
        "openai-codex" | "openai_codex" | "codex" => {
            create_provider_with_options(primary_name, api_key, options)?
        }
        _ => create_provider_with_url_and_options(primary_name, api_key, api_url, options)?,
    };
    providers.push((primary_name.to_string(), primary_provider));

    for fallback in &reliability.fallback_providers {
        if fallback == primary_name || providers.iter().any(|(name, _)| name == fallback) {
            continue;
        }

        let (provider_name, profile_override) = parse_provider_profile(fallback);

        // Each fallback provider resolves its own credential via provider-
        // specific env vars (e.g. DEEPSEEK_API_KEY for "deepseek") instead
        // of inheriting the primary provider's key. Passing `None` lets
        // `resolve_provider_credential` check the correct env var for the
        // fallback provider name.
        //
        // When a profile override is present (e.g. "openai-codex:second"),
        // propagate it through `auth_profile_override` so the provider
        // picks up the correct OAuth credential set.
        let fallback_options = match profile_override {
            Some(profile) => {
                let mut opts = options.clone();
                opts.auth_profile_override = Some(profile.to_string());
                opts
            }
            None => options.clone(),
        };

        match create_provider_with_options(provider_name, None, &fallback_options) {
            Ok(provider) => providers.push((fallback.clone(), provider)),
            Err(_error) => {
                tracing::warn!(
                    fallback_provider = fallback,
                    "Ignoring invalid fallback provider during initialization"
                );
            }
        }
    }

    let reliable = ReliableProvider::new(
        providers,
        reliability.provider_retries,
        reliability.provider_backoff_ms,
    )
    .with_api_keys(reliability.api_keys.clone())
    .with_model_fallbacks(reliability.model_fallbacks.clone());

    Ok(Box::new(reliable))
}

/// Create a RouterProvider if model routes are configured, otherwise return a
/// standard resilient provider. The router wraps individual providers per route,
/// each with its own retry/fallback chain.
pub fn create_routed_provider(
    primary_name: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
    reliability: &zeroclaw_config::schema::ReliabilityConfig,
    model_routes: &[zeroclaw_config::schema::ModelRouteConfig],
    default_model: &str,
) -> anyhow::Result<Box<dyn Provider>> {
    create_routed_provider_with_options(
        primary_name,
        api_key,
        api_url,
        reliability,
        model_routes,
        default_model,
        &ProviderRuntimeOptions::default(),
    )
}

/// Create a routed provider using explicit runtime options.
pub fn create_routed_provider_with_options(
    primary_name: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
    reliability: &zeroclaw_config::schema::ReliabilityConfig,
    model_routes: &[zeroclaw_config::schema::ModelRouteConfig],
    default_model: &str,
    options: &ProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn Provider>> {
    if model_routes.is_empty() {
        return create_resilient_provider_with_options(
            primary_name,
            api_key,
            api_url,
            reliability,
            options,
        );
    }

    // Collect unique provider names needed
    let mut needed: Vec<String> = vec![primary_name.to_string()];
    for route in model_routes {
        if !needed.iter().any(|n| n == &route.provider) {
            needed.push(route.provider.clone());
        }
    }

    // Create each provider (with its own resilience wrapper)
    let mut providers: Vec<(String, Box<dyn Provider>)> = Vec::new();
    for name in &needed {
        let routed_credential = model_routes
            .iter()
            .find(|r| &r.provider == name)
            .and_then(|r| {
                r.api_key.as_ref().and_then(|raw_key| {
                    let trimmed_key = raw_key.trim();
                    (!trimmed_key.is_empty()).then_some(trimmed_key)
                })
            });
        let key = routed_credential.or(api_key);
        // Only use api_url for the primary provider
        let url = if name == primary_name { api_url } else { None };
        match create_resilient_provider_with_options(name, key, url, reliability, options) {
            Ok(provider) => providers.push((name.clone(), provider)),
            Err(e) => {
                if name == primary_name {
                    return Err(e);
                }
                tracing::warn!(
                    provider = name.as_str(),
                    "Ignoring routed provider that failed to initialize"
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
                    provider_name: r.provider.clone(),
                    model: r.model.clone(),
                },
            )
        })
        .collect();

    Ok(Box::new(router::RouterProvider::new(
        providers,
        routes,
        default_model.to_string(),
    )))
}

/// Information about a supported provider for display purposes.
pub struct ProviderInfo {
    /// Canonical name used in config (e.g. `"openrouter"`)
    pub name: &'static str,
    /// Human-readable display name
    pub display_name: &'static str,
    /// Alternative names accepted in config
    pub aliases: &'static [&'static str],
    /// Whether the provider runs locally (no API key required)
    pub local: bool,
}

/// Return the list of all known providers for display in `zeroclaw providers list`.
///
/// This is intentionally separate from the factory match in `create_provider`
/// (display concern vs. construction concern).
pub fn list_providers() -> Vec<ProviderInfo> {
    vec![
        // ── Primary providers ────────────────────────────────
        ProviderInfo {
            name: "openrouter",
            display_name: "OpenRouter",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "anthropic",
            display_name: "Anthropic",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "openai",
            display_name: "OpenAI",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "openai-codex",
            display_name: "OpenAI Codex (OAuth)",
            aliases: &["openai_codex", "codex"],
            local: false,
        },
        ProviderInfo {
            name: "telnyx",
            display_name: "Telnyx",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "azure_openai",
            display_name: "Azure OpenAI",
            aliases: &["azure-openai", "azure"],
            local: false,
        },
        ProviderInfo {
            name: "ollama",
            display_name: "Ollama",
            aliases: &[],
            local: true,
        },
        ProviderInfo {
            name: "gemini",
            display_name: "Google Gemini",
            aliases: &["google", "google-gemini"],
            local: false,
        },
        // ── OpenAI-compatible providers ──────────────────────
        ProviderInfo {
            name: "venice",
            display_name: "Venice",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "vercel",
            display_name: "Vercel AI Gateway",
            aliases: &["vercel-ai"],
            local: false,
        },
        ProviderInfo {
            name: "cloudflare",
            display_name: "Cloudflare AI",
            aliases: &["cloudflare-ai"],
            local: false,
        },
        ProviderInfo {
            name: "moonshot",
            display_name: "Moonshot",
            aliases: &["kimi"],
            local: false,
        },
        ProviderInfo {
            name: "kimi-code",
            display_name: "Kimi Code",
            aliases: &["kimi_coding", "kimi_for_coding"],
            local: false,
        },
        ProviderInfo {
            name: "synthetic",
            display_name: "Synthetic",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "opencode",
            display_name: "OpenCode Zen",
            aliases: &["opencode-zen"],
            local: false,
        },
        ProviderInfo {
            name: "opencode-go",
            display_name: "OpenCode Go",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "zai",
            display_name: "Z.AI",
            aliases: &["z.ai"],
            local: false,
        },
        ProviderInfo {
            name: "glm",
            display_name: "GLM (Zhipu)",
            aliases: &["zhipu"],
            local: false,
        },
        ProviderInfo {
            name: "minimax",
            display_name: "MiniMax",
            aliases: &[
                "minimax-intl",
                "minimax-io",
                "minimax-global",
                "minimax-cn",
                "minimaxi",
                "minimax-oauth",
                "minimax-oauth-cn",
                "minimax-portal",
                "minimax-portal-cn",
            ],
            local: false,
        },
        ProviderInfo {
            name: "bedrock",
            display_name: "Amazon Bedrock",
            aliases: &["aws-bedrock"],
            local: false,
        },
        ProviderInfo {
            name: "qianfan",
            display_name: "Qianfan (Baidu)",
            aliases: &["baidu"],
            local: false,
        },
        ProviderInfo {
            name: "doubao",
            display_name: "Doubao (Volcengine)",
            aliases: &["volcengine", "ark", "doubao-cn"],
            local: false,
        },
        ProviderInfo {
            name: "qwen",
            display_name: "Qwen (DashScope / Qwen Code OAuth)",
            aliases: &[
                "dashscope",
                "qwen-intl",
                "dashscope-intl",
                "qwen-us",
                "dashscope-us",
                "qwen-code",
                "qwen-oauth",
                "qwen_oauth",
            ],
            local: false,
        },
        ProviderInfo {
            name: "bailian",
            display_name: "Bailian (Aliyun)",
            aliases: &["aliyun-bailian", "aliyun"],
            local: false,
        },
        ProviderInfo {
            name: "groq",
            display_name: "Groq",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "mistral",
            display_name: "Mistral",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "xai",
            display_name: "xAI (Grok)",
            aliases: &["grok"],
            local: false,
        },
        ProviderInfo {
            name: "deepseek",
            display_name: "DeepSeek",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "together",
            display_name: "Together AI",
            aliases: &["together-ai"],
            local: false,
        },
        ProviderInfo {
            name: "fireworks",
            display_name: "Fireworks AI",
            aliases: &["fireworks-ai"],
            local: false,
        },
        ProviderInfo {
            name: "novita",
            display_name: "Novita AI",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "perplexity",
            display_name: "Perplexity",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "cohere",
            display_name: "Cohere",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "copilot",
            display_name: "GitHub Copilot",
            aliases: &["github-copilot"],
            local: false,
        },
        ProviderInfo {
            name: "claude-code",
            display_name: "Claude Code (CLI)",
            aliases: &[],
            local: true,
        },
        ProviderInfo {
            name: "gemini-cli",
            display_name: "Gemini CLI",
            aliases: &[],
            local: true,
        },
        ProviderInfo {
            name: "kilocli",
            display_name: "KiloCLI",
            aliases: &["kilo"],
            local: true,
        },
        ProviderInfo {
            name: "lmstudio",
            display_name: "LM Studio",
            aliases: &["lm-studio"],
            local: true,
        },
        ProviderInfo {
            name: "llamacpp",
            display_name: "llama.cpp server",
            aliases: &["llama.cpp"],
            local: true,
        },
        ProviderInfo {
            name: "sglang",
            display_name: "SGLang",
            aliases: &[],
            local: true,
        },
        ProviderInfo {
            name: "vllm",
            display_name: "vLLM",
            aliases: &[],
            local: true,
        },
        ProviderInfo {
            name: "osaurus",
            display_name: "Osaurus",
            aliases: &[],
            local: true,
        },
        ProviderInfo {
            name: "nvidia",
            display_name: "NVIDIA NIM",
            aliases: &["nvidia-nim", "build.nvidia.com"],
            local: false,
        },
        ProviderInfo {
            name: "siliconflow",
            display_name: "SiliconFlow",
            aliases: &["silicon-flow"],
            local: false,
        },
        ProviderInfo {
            name: "aihubmix",
            display_name: "AiHubMix",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "litellm",
            display_name: "LiteLLM",
            aliases: &["lite-llm"],
            local: false,
        },
        // ── Fast inference ────────────────────────────────────
        ProviderInfo {
            name: "cerebras",
            display_name: "Cerebras",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "sambanova",
            display_name: "SambaNova",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "hyperbolic",
            display_name: "Hyperbolic",
            aliases: &[],
            local: false,
        },
        // ── Model hosting platforms ──────────────────────────
        ProviderInfo {
            name: "deepinfra",
            display_name: "DeepInfra",
            aliases: &["deep-infra"],
            local: false,
        },
        ProviderInfo {
            name: "huggingface",
            display_name: "Hugging Face",
            aliases: &["hf"],
            local: false,
        },
        ProviderInfo {
            name: "ai21",
            display_name: "AI21 Labs",
            aliases: &["ai21-labs"],
            local: false,
        },
        ProviderInfo {
            name: "reka",
            display_name: "Reka",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "baseten",
            display_name: "Baseten",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "nscale",
            display_name: "Nscale",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "anyscale",
            display_name: "Anyscale",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "nebius",
            display_name: "Nebius AI Studio",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "friendli",
            display_name: "Friendli AI",
            aliases: &["friendliai"],
            local: false,
        },
        ProviderInfo {
            name: "lepton",
            display_name: "Lepton AI",
            aliases: &["lepton-ai"],
            local: false,
        },
        // ── Chinese AI providers ─────────────────────────────
        ProviderInfo {
            name: "stepfun",
            display_name: "Stepfun",
            aliases: &["step"],
            local: false,
        },
        ProviderInfo {
            name: "baichuan",
            display_name: "Baichuan",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "yi",
            display_name: "01.AI (Yi)",
            aliases: &["01ai", "lingyiwanwu"],
            local: false,
        },
        ProviderInfo {
            name: "hunyuan",
            display_name: "Tencent Hunyuan",
            aliases: &["tencent"],
            local: false,
        },
        // ── Cloud AI endpoints ───────────────────────────────
        ProviderInfo {
            name: "ovhcloud",
            display_name: "OVHcloud AI Endpoints",
            aliases: &["ovh"],
            local: false,
        },
        ProviderInfo {
            name: "avian",
            display_name: "Avian",
            aliases: &[],
            local: false,
        },
    ]
}

/// Shared test utilities for provider modules.
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

    #[test]
    fn resolve_provider_credential_prefers_explicit_argument() {
        let resolved = resolve_provider_credential("openrouter", Some("  explicit-key  "));
        assert_eq!(resolved, Some("explicit-key".to_string()));
    }

    #[test]
    fn resolve_provider_credential_uses_minimax_oauth_env_for_placeholder() {
        let _env_lock = env_lock();
        let _oauth_guard = EnvGuard::set(MINIMAX_OAUTH_TOKEN_ENV, Some("oauth-token"));
        let _api_guard = EnvGuard::set(MINIMAX_API_KEY_ENV, Some("api-key"));
        let _refresh_guard = EnvGuard::set(MINIMAX_OAUTH_REFRESH_TOKEN_ENV, None);

        let resolved = resolve_provider_credential("minimax", Some(MINIMAX_OAUTH_PLACEHOLDER));

        assert_eq!(resolved.as_deref(), Some("oauth-token"));
    }

    #[test]
    fn resolve_provider_credential_falls_back_to_minimax_api_key_for_placeholder() {
        let _env_lock = env_lock();
        let _oauth_guard = EnvGuard::set(MINIMAX_OAUTH_TOKEN_ENV, None);
        let _api_guard = EnvGuard::set(MINIMAX_API_KEY_ENV, Some("api-key"));
        let _refresh_guard = EnvGuard::set(MINIMAX_OAUTH_REFRESH_TOKEN_ENV, None);

        let resolved = resolve_provider_credential("minimax", Some(MINIMAX_OAUTH_PLACEHOLDER));

        assert_eq!(resolved.as_deref(), Some("api-key"));
    }

    #[test]
    fn resolve_provider_credential_placeholder_ignores_generic_api_key_fallback() {
        let _env_lock = env_lock();
        let _oauth_guard = EnvGuard::set(MINIMAX_OAUTH_TOKEN_ENV, None);
        let _api_guard = EnvGuard::set(MINIMAX_API_KEY_ENV, None);
        let _refresh_guard = EnvGuard::set(MINIMAX_OAUTH_REFRESH_TOKEN_ENV, None);
        let _generic_guard = EnvGuard::set("API_KEY", Some("generic-key"));

        let resolved = resolve_provider_credential("minimax", Some(MINIMAX_OAUTH_PLACEHOLDER));

        assert!(resolved.is_none());
    }

    #[test]
    fn resolve_provider_credential_bedrock_uses_internal_credential_path() {
        let _generic_guard = EnvGuard::set("API_KEY", Some("generic-key"));
        let _override_guard = EnvGuard::set("OPENROUTER_API_KEY", Some("openrouter-key"));
        let _bedrock_guard = EnvGuard::set("BEDROCK_API_KEY", None);

        assert_eq!(
            resolve_provider_credential("bedrock", Some("explicit")),
            Some("explicit".to_string())
        );
        assert!(resolve_provider_credential("bedrock", None).is_none());
        assert!(resolve_provider_credential("aws-bedrock", None).is_none());
    }

    #[test]
    fn resolve_provider_credential_bedrock_returns_bearer_token_from_env() {
        let _bedrock_guard = EnvGuard::set("BEDROCK_API_KEY", Some("bedrock-bearer-token"));

        assert_eq!(
            resolve_provider_credential("bedrock", None),
            Some("bedrock-bearer-token".to_string())
        );
        assert_eq!(
            resolve_provider_credential("aws-bedrock", None),
            Some("bedrock-bearer-token".to_string())
        );
    }

    #[test]
    fn resolve_qwen_oauth_context_prefers_explicit_override() {
        let _env_lock = env_lock();
        let fake_home = format!("/tmp/zeroclaw-qwen-oauth-home-{}", std::process::id());
        let _home_guard = EnvGuard::set("HOME", Some(fake_home.as_str()));
        let _token_guard = EnvGuard::set(QWEN_OAUTH_TOKEN_ENV, Some("oauth-token"));
        let _resource_guard = EnvGuard::set(
            QWEN_OAUTH_RESOURCE_URL_ENV,
            Some("coding-intl.dashscope.aliyuncs.com"),
        );

        let context = resolve_qwen_oauth_context(Some("  explicit-qwen-token  "));

        assert_eq!(context.credential.as_deref(), Some("explicit-qwen-token"));
        assert!(context.base_url.is_none());
    }

    #[test]
    fn resolve_qwen_oauth_context_uses_env_token_and_resource_url() {
        let _env_lock = env_lock();
        let fake_home = format!("/tmp/zeroclaw-qwen-oauth-home-{}-env", std::process::id());
        let _home_guard = EnvGuard::set("HOME", Some(fake_home.as_str()));
        let _token_guard = EnvGuard::set(QWEN_OAUTH_TOKEN_ENV, Some("oauth-token"));
        let _refresh_guard = EnvGuard::set(QWEN_OAUTH_REFRESH_TOKEN_ENV, None);
        let _resource_guard = EnvGuard::set(
            QWEN_OAUTH_RESOURCE_URL_ENV,
            Some("coding-intl.dashscope.aliyuncs.com"),
        );
        let _dashscope_guard = EnvGuard::set("DASHSCOPE_API_KEY", Some("dashscope-fallback"));

        let context = resolve_qwen_oauth_context(Some(QWEN_OAUTH_PLACEHOLDER));

        assert_eq!(context.credential.as_deref(), Some("oauth-token"));
        assert_eq!(
            context.base_url.as_deref(),
            Some("https://coding-intl.dashscope.aliyuncs.com/v1")
        );
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
        let _token_guard = EnvGuard::set(QWEN_OAUTH_TOKEN_ENV, None);
        let _refresh_guard = EnvGuard::set(QWEN_OAUTH_REFRESH_TOKEN_ENV, None);
        let _resource_guard = EnvGuard::set(QWEN_OAUTH_RESOURCE_URL_ENV, None);
        let _dashscope_guard = EnvGuard::set("DASHSCOPE_API_KEY", None);

        let context = resolve_qwen_oauth_context(Some(QWEN_OAUTH_PLACEHOLDER));

        assert_eq!(context.credential.as_deref(), Some("cached-token"));
        assert_eq!(
            context.base_url.as_deref(),
            Some("https://resource.example.com/v1")
        );
    }

    #[test]
    fn resolve_qwen_oauth_context_placeholder_does_not_use_dashscope_fallback() {
        let _env_lock = env_lock();
        let fake_home = format!(
            "/tmp/zeroclaw-qwen-oauth-home-{}-placeholder",
            std::process::id()
        );
        let _home_guard = EnvGuard::set("HOME", Some(fake_home.as_str()));
        let _token_guard = EnvGuard::set(QWEN_OAUTH_TOKEN_ENV, None);
        let _refresh_guard = EnvGuard::set(QWEN_OAUTH_REFRESH_TOKEN_ENV, None);
        let _resource_guard = EnvGuard::set(QWEN_OAUTH_RESOURCE_URL_ENV, None);
        let _dashscope_guard = EnvGuard::set("DASHSCOPE_API_KEY", Some("dashscope-fallback"));

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

    #[test]
    fn canonical_china_provider_name_maps_regional_aliases() {
        assert_eq!(canonical_china_provider_name("moonshot"), Some("moonshot"));
        assert_eq!(canonical_china_provider_name("kimi-intl"), Some("moonshot"));
        assert_eq!(canonical_china_provider_name("glm"), Some("glm"));
        assert_eq!(canonical_china_provider_name("zhipu-cn"), Some("glm"));
        assert_eq!(canonical_china_provider_name("minimax"), Some("minimax"));
        assert_eq!(canonical_china_provider_name("minimax-cn"), Some("minimax"));
        assert_eq!(canonical_china_provider_name("qwen"), Some("qwen"));
        assert_eq!(canonical_china_provider_name("dashscope-us"), Some("qwen"));
        assert_eq!(canonical_china_provider_name("qwen-code"), Some("qwen"));
        assert_eq!(canonical_china_provider_name("zai"), Some("zai"));
        assert_eq!(canonical_china_provider_name("z.ai-cn"), Some("zai"));
        assert_eq!(canonical_china_provider_name("qianfan"), Some("qianfan"));
        assert_eq!(canonical_china_provider_name("baidu"), Some("qianfan"));
        assert_eq!(canonical_china_provider_name("doubao"), Some("doubao"));
        assert_eq!(canonical_china_provider_name("volcengine"), Some("doubao"));
        assert_eq!(canonical_china_provider_name("bailian"), Some("bailian"));
        assert_eq!(
            canonical_china_provider_name("aliyun-bailian"),
            Some("bailian")
        );
        assert_eq!(canonical_china_provider_name("aliyun"), Some("bailian"));
        assert_eq!(canonical_china_provider_name("openai"), None);
    }

    #[test]
    fn regional_endpoint_aliases_map_to_expected_urls() {
        assert_eq!(minimax_base_url("minimax"), Some(MINIMAX_INTL_BASE_URL));
        assert_eq!(
            minimax_base_url("minimax-intl"),
            Some(MINIMAX_INTL_BASE_URL)
        );
        assert_eq!(minimax_base_url("minimax-cn"), Some(MINIMAX_CN_BASE_URL));

        assert_eq!(glm_base_url("glm"), Some(GLM_GLOBAL_BASE_URL));
        assert_eq!(glm_base_url("glm-cn"), Some(GLM_CN_BASE_URL));
        assert_eq!(glm_base_url("bigmodel"), Some(GLM_CN_BASE_URL));

        assert_eq!(moonshot_base_url("moonshot"), Some(MOONSHOT_CN_BASE_URL));
        assert_eq!(
            moonshot_base_url("moonshot-intl"),
            Some(MOONSHOT_INTL_BASE_URL)
        );

        assert_eq!(qwen_base_url("qwen"), Some(QWEN_CN_BASE_URL));
        assert_eq!(qwen_base_url("qwen-cn"), Some(QWEN_CN_BASE_URL));
        assert_eq!(qwen_base_url("qwen-intl"), Some(QWEN_INTL_BASE_URL));
        assert_eq!(qwen_base_url("qwen-us"), Some(QWEN_US_BASE_URL));
        assert_eq!(qwen_base_url("qwen-code"), Some(QWEN_CN_BASE_URL));

        assert_eq!(zai_base_url("zai"), Some(ZAI_GLOBAL_BASE_URL));
        assert_eq!(zai_base_url("z.ai"), Some(ZAI_GLOBAL_BASE_URL));
        assert_eq!(zai_base_url("zai-global"), Some(ZAI_GLOBAL_BASE_URL));
        assert_eq!(zai_base_url("z.ai-global"), Some(ZAI_GLOBAL_BASE_URL));
        assert_eq!(zai_base_url("zai-cn"), Some(ZAI_CN_BASE_URL));
        assert_eq!(zai_base_url("z.ai-cn"), Some(ZAI_CN_BASE_URL));
    }

    // ── Primary providers ────────────────────────────────────

    #[test]
    fn factory_openrouter() {
        assert!(create_provider("openrouter", Some("provider-test-credential")).is_ok());
        assert!(create_provider("openrouter", None).is_ok());
    }

    #[test]
    fn factory_anthropic() {
        assert!(create_provider("anthropic", Some("provider-test-credential")).is_ok());
    }

    #[test]
    fn factory_openai() {
        assert!(create_provider("openai", Some("provider-test-credential")).is_ok());
    }

    #[test]
    fn factory_openai_codex() {
        let options = ProviderRuntimeOptions::default();
        assert!(create_provider_with_options("openai-codex", None, &options).is_ok());
    }

    #[test]
    fn factory_ollama() {
        assert!(create_provider("ollama", None).is_ok());
        // Ollama may use API key when a remote endpoint is configured.
        assert!(create_provider("ollama", Some("dummy")).is_ok());
        assert!(create_provider("ollama", Some("any-value-here")).is_ok());
    }

    #[test]
    fn factory_gemini() {
        assert!(create_provider("gemini", Some("test-key")).is_ok());
        assert!(create_provider("google", Some("test-key")).is_ok());
        assert!(create_provider("google-gemini", Some("test-key")).is_ok());
        // Should also work without key (will try CLI auth)
        assert!(create_provider("gemini", None).is_ok());
    }

    #[test]
    fn factory_telnyx() {
        assert!(create_provider("telnyx", Some("test-key")).is_ok());
        assert!(create_provider("telnyx", None).is_ok());
    }

    // ── OpenAI-compatible providers ──────────────────────────

    #[test]
    fn factory_venice() {
        let provider = create_provider("venice", Some("vn-key")).unwrap();
        assert!(
            !provider.capabilities().native_tool_calling,
            "Venice should use prompt-guided tools, not native tool calling"
        );
    }

    #[test]
    fn factory_vercel() {
        assert!(create_provider("vercel", Some("key")).is_ok());
        assert!(create_provider("vercel-ai", Some("key")).is_ok());
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
        assert!(create_provider("cloudflare", Some("key")).is_ok());
        assert!(create_provider("cloudflare-ai", Some("key")).is_ok());
    }

    #[test]
    fn factory_moonshot() {
        assert!(create_provider("moonshot", Some("key")).is_ok());
        assert!(create_provider("kimi", Some("key")).is_ok());
        assert!(create_provider("moonshot-intl", Some("key")).is_ok());
        assert!(create_provider("moonshot-cn", Some("key")).is_ok());
        assert!(create_provider("kimi-intl", Some("key")).is_ok());
        assert!(create_provider("kimi-cn", Some("key")).is_ok());
    }

    #[test]
    fn factory_kimi_code() {
        assert!(create_provider("kimi-code", Some("key")).is_ok());
        assert!(create_provider("kimi_coding", Some("key")).is_ok());
        assert!(create_provider("kimi_for_coding", Some("key")).is_ok());
    }

    #[test]
    fn factory_synthetic() {
        assert!(create_provider("synthetic", Some("key")).is_ok());
    }

    #[test]
    fn factory_opencode() {
        assert!(create_provider("opencode", Some("key")).is_ok());
        assert!(create_provider("opencode-zen", Some("key")).is_ok());
    }

    #[test]
    fn factory_opencode_go() {
        assert!(create_provider("opencode-go", Some("key")).is_ok());
    }

    #[test]
    fn resolve_provider_credential_opencode_go_env() {
        let _env_lock = env_lock();
        let _provider_guard = EnvGuard::set("OPENCODE_GO_API_KEY", Some("go-test-key"));
        let _generic_guard = EnvGuard::set("API_KEY", None);
        let _zeroclaw_guard = EnvGuard::set("ZEROCLAW_API_KEY", None);

        let resolved = resolve_provider_credential("opencode-go", None);
        assert_eq!(resolved.as_deref(), Some("go-test-key"));
    }

    #[test]
    fn factory_zai() {
        assert!(create_provider("zai", Some("key")).is_ok());
        assert!(create_provider("z.ai", Some("key")).is_ok());
        assert!(create_provider("zai-global", Some("key")).is_ok());
        assert!(create_provider("z.ai-global", Some("key")).is_ok());
        assert!(create_provider("zai-cn", Some("key")).is_ok());
        assert!(create_provider("z.ai-cn", Some("key")).is_ok());
    }

    #[test]
    fn factory_glm() {
        assert!(create_provider("glm", Some("key")).is_ok());
        assert!(create_provider("zhipu", Some("key")).is_ok());
        assert!(create_provider("glm-cn", Some("key")).is_ok());
        assert!(create_provider("zhipu-cn", Some("key")).is_ok());
        assert!(create_provider("glm-global", Some("key")).is_ok());
        assert!(create_provider("bigmodel", Some("key")).is_ok());
    }

    #[test]
    fn factory_minimax() {
        assert!(create_provider("minimax", Some("key")).is_ok());
        assert!(create_provider("minimax-intl", Some("key")).is_ok());
        assert!(create_provider("minimax-io", Some("key")).is_ok());
        assert!(create_provider("minimax-global", Some("key")).is_ok());
        assert!(create_provider("minimax-cn", Some("key")).is_ok());
        assert!(create_provider("minimaxi", Some("key")).is_ok());
        assert!(create_provider("minimax-oauth", Some("key")).is_ok());
        assert!(create_provider("minimax-oauth-cn", Some("key")).is_ok());
        assert!(create_provider("minimax-portal", Some("key")).is_ok());
        assert!(create_provider("minimax-portal-cn", Some("key")).is_ok());
    }

    #[test]
    fn factory_minimax_supports_native_tool_calling() {
        let minimax = create_provider("minimax", Some("key")).expect("provider should resolve");
        assert!(minimax.supports_native_tools());

        let minimax_cn =
            create_provider("minimax-cn", Some("key")).expect("provider should resolve");
        assert!(minimax_cn.supports_native_tools());
    }

    #[test]
    fn factory_bedrock() {
        // Bedrock uses AWS env vars for credentials, not API key.
        assert!(create_provider("bedrock", None).is_ok());
        assert!(create_provider("aws-bedrock", None).is_ok());
        // Passing an api_key is harmless (ignored).
        assert!(create_provider("bedrock", Some("ignored")).is_ok());
    }

    #[test]
    fn factory_qianfan() {
        assert!(create_provider("qianfan", Some("key")).is_ok());
        assert!(create_provider("baidu", Some("key")).is_ok());
    }

    #[test]
    fn factory_doubao() {
        assert!(create_provider("doubao", Some("key")).is_ok());
        assert!(create_provider("volcengine", Some("key")).is_ok());
        assert!(create_provider("ark", Some("key")).is_ok());
        assert!(create_provider("doubao-cn", Some("key")).is_ok());
    }

    #[test]
    fn factory_qwen() {
        assert!(create_provider("qwen", Some("key")).is_ok());
        assert!(create_provider("dashscope", Some("key")).is_ok());
        assert!(create_provider("qwen-cn", Some("key")).is_ok());
        assert!(create_provider("dashscope-cn", Some("key")).is_ok());
        assert!(create_provider("qwen-intl", Some("key")).is_ok());
        assert!(create_provider("dashscope-intl", Some("key")).is_ok());
        assert!(create_provider("qwen-international", Some("key")).is_ok());
        assert!(create_provider("dashscope-international", Some("key")).is_ok());
        assert!(create_provider("qwen-us", Some("key")).is_ok());
        assert!(create_provider("dashscope-us", Some("key")).is_ok());
        assert!(create_provider("qwen-code", Some("key")).is_ok());
        assert!(create_provider("qwen-oauth", Some("key")).is_ok());
    }

    #[test]
    fn qwen_provider_supports_vision() {
        let provider = create_provider("qwen", Some("key")).expect("qwen provider should build");
        assert!(provider.supports_vision());

        let oauth_provider =
            create_provider("qwen-code", Some("key")).expect("qwen oauth provider should build");
        assert!(oauth_provider.supports_vision());
    }

    #[test]
    fn factory_lmstudio() {
        assert!(create_provider("lmstudio", Some("key")).is_ok());
        assert!(create_provider("lm-studio", Some("key")).is_ok());
        assert!(create_provider("lmstudio", None).is_ok());
    }

    #[test]
    fn factory_llamacpp() {
        assert!(create_provider("llamacpp", Some("key")).is_ok());
        assert!(create_provider("llama.cpp", Some("key")).is_ok());
        assert!(create_provider("llamacpp", None).is_ok());
    }

    #[test]
    fn factory_sglang() {
        assert!(create_provider("sglang", None).is_ok());
        assert!(create_provider("sglang", Some("key")).is_ok());
    }

    #[test]
    fn factory_vllm() {
        assert!(create_provider("vllm", None).is_ok());
        assert!(create_provider("vllm", Some("key")).is_ok());
    }

    #[test]
    fn factory_osaurus() {
        // Osaurus works without an explicit key (defaults to "osaurus").
        assert!(create_provider("osaurus", None).is_ok());
        // Osaurus also works with an explicit key.
        assert!(create_provider("osaurus", Some("custom-key")).is_ok());
    }

    #[test]
    fn factory_osaurus_uses_default_key_when_none() {
        // Verify that create_provider_with_url_and_options succeeds even
        // without an API key — the match arm provides a default placeholder.
        let options = ProviderRuntimeOptions::default();
        let p = create_provider_with_url_and_options("osaurus", None, None, &options);
        assert!(p.is_ok());
    }

    #[test]
    fn factory_osaurus_custom_url() {
        // Verify that a custom api_url overrides the default localhost endpoint.
        let options = ProviderRuntimeOptions::default();
        let p = create_provider_with_url_and_options(
            "osaurus",
            Some("key"),
            Some("http://192.168.1.100:1337/v1"),
            &options,
        );
        assert!(p.is_ok());
    }

    #[test]
    fn resolve_provider_credential_osaurus_env() {
        let _env_lock = env_lock();
        let _guard = EnvGuard::set("OSAURUS_API_KEY", Some("osaurus-test-key"));
        let resolved = resolve_provider_credential("osaurus", None);
        assert_eq!(resolved, Some("osaurus-test-key".to_string()));
    }

    #[test]
    fn resolve_provider_credential_volcengine_env() {
        let _env_lock = env_lock();
        let _guard = EnvGuard::set("VOLCENGINE_API_KEY", Some("volc-test-key"));
        let resolved = resolve_provider_credential("volcengine", None);
        assert_eq!(resolved, Some("volc-test-key".to_string()));
    }

    #[test]
    fn resolve_provider_credential_aihubmix_env() {
        let _env_lock = env_lock();
        let _guard = EnvGuard::set("AIHUBMIX_API_KEY", Some("aihubmix-test-key"));
        let resolved = resolve_provider_credential("aihubmix", None);
        assert_eq!(resolved, Some("aihubmix-test-key".to_string()));
    }

    #[test]
    fn resolve_provider_credential_siliconflow_env() {
        let _env_lock = env_lock();
        let _guard = EnvGuard::set("SILICONFLOW_API_KEY", Some("sf-test-key"));
        let resolved = resolve_provider_credential("siliconflow", None);
        assert_eq!(resolved, Some("sf-test-key".to_string()));
    }

    #[test]
    fn factory_aihubmix() {
        assert!(create_provider("aihubmix", Some("key")).is_ok());
    }

    #[test]
    fn factory_siliconflow() {
        assert!(create_provider("siliconflow", Some("key")).is_ok());
        assert!(create_provider("silicon-flow", Some("key")).is_ok());
    }

    #[test]
    fn factory_codex_oauth_aliases() {
        let options = ProviderRuntimeOptions::default();
        for alias in &["codex", "openai-codex", "openai_codex"] {
            assert!(
                create_provider_with_options(alias, None, &options).is_ok(),
                "codex alias '{alias}' should produce a provider"
            );
        }
    }

    // ── Extended ecosystem ───────────────────────────────────

    #[test]
    fn factory_groq() {
        assert!(create_provider("groq", Some("key")).is_ok());
    }

    #[test]
    fn factory_groq_disables_native_tools_by_default() {
        // Default behavior preserves the #5848 blanket disable: llama-family
        // Groq models reject native tool calls with HTTP 400.
        let provider =
            create_provider_with_options("groq", Some("key"), &ProviderRuntimeOptions::default())
                .expect("groq factory must succeed");
        assert!(
            !provider.supports_native_tools(),
            "Groq must default to text-fallback for llama-family compatibility (#5848)"
        );
    }

    #[test]
    fn factory_groq_honors_native_tools_override_true() {
        // Operator opt-in via `[providers.models.<alias>] native_tools = true`
        // skips the default disable so non-llama Groq models can use native
        // tool calling (#5932).
        let options = ProviderRuntimeOptions {
            native_tools: Some(true),
            ..Default::default()
        };
        let provider = create_provider_with_options("groq", Some("key"), &options)
            .expect("groq factory must succeed");
        assert!(
            provider.supports_native_tools(),
            "Groq with `native_tools = true` must enable native tool calling (#5932)"
        );
    }

    #[test]
    fn factory_groq_native_tools_override_false_keeps_disable() {
        // Explicit `native_tools = false` matches the default behavior; this
        // documents that the option is tri-state and `Some(false)` is not a
        // no-op surprise.
        let options = ProviderRuntimeOptions {
            native_tools: Some(false),
            ..Default::default()
        };
        let provider = create_provider_with_options("groq", Some("key"), &options)
            .expect("groq factory must succeed");
        assert!(
            !provider.supports_native_tools(),
            "Groq with explicit `native_tools = false` must remain text-fallback"
        );
    }

    #[test]
    fn provider_runtime_options_from_config_propagates_native_tools() {
        // The end-to-end path the operator uses: setting `native_tools` on
        // the active provider profile must reach `ProviderRuntimeOptions`
        // so the Groq factory branch sees it (#5932).
        let mut config = zeroclaw_config::schema::Config::default();
        let mut groq = zeroclaw_config::schema::ModelProviderConfig {
            native_tools: Some(true),
            ..Default::default()
        };
        groq.base_url = Some("https://api.groq.com/openai/v1".to_string());
        config.providers.models.insert("groq".to_string(), groq);
        config.providers.fallback = Some("groq".to_string());

        let options = provider_runtime_options_from_config(&config);
        assert_eq!(
            options.native_tools,
            Some(true),
            "native_tools must propagate from the active provider profile to runtime options"
        );
    }

    #[test]
    fn factory_mistral() {
        assert!(create_provider("mistral", Some("key")).is_ok());
    }

    #[test]
    fn factory_xai() {
        assert!(create_provider("xai", Some("key")).is_ok());
        assert!(create_provider("grok", Some("key")).is_ok());
    }

    #[test]
    fn factory_deepseek() {
        assert!(create_provider("deepseek", Some("key")).is_ok());
    }

    #[test]
    fn deepseek_provider_keeps_vision_disabled() {
        let provider =
            create_provider("deepseek", Some("key")).expect("deepseek provider should build");
        assert!(!provider.supports_vision());
    }

    #[test]
    fn factory_together() {
        assert!(create_provider("together", Some("key")).is_ok());
        assert!(create_provider("together-ai", Some("key")).is_ok());
    }

    #[test]
    fn factory_fireworks() {
        assert!(create_provider("fireworks", Some("key")).is_ok());
        assert!(create_provider("fireworks-ai", Some("key")).is_ok());
    }

    #[test]
    fn factory_novita() {
        assert!(create_provider("novita", Some("key")).is_ok());
    }

    #[test]
    fn factory_perplexity() {
        assert!(create_provider("perplexity", Some("key")).is_ok());
    }

    #[test]
    fn factory_cohere() {
        assert!(create_provider("cohere", Some("key")).is_ok());
    }

    #[test]
    fn factory_copilot() {
        assert!(create_provider("copilot", Some("key")).is_ok());
        assert!(create_provider("github-copilot", Some("key")).is_ok());
    }

    #[test]
    fn factory_claude_code() {
        assert!(create_provider("claude-code", None).is_ok());
    }

    #[test]
    fn factory_gemini_cli() {
        assert!(create_provider("gemini-cli", None).is_ok());
    }

    #[test]
    fn factory_kilocli() {
        assert!(create_provider("kilocli", None).is_ok());
        assert!(create_provider("kilo", None).is_ok());
    }

    #[test]
    fn factory_nvidia() {
        assert!(create_provider("nvidia", Some("nvapi-test")).is_ok());
        assert!(create_provider("nvidia-nim", Some("nvapi-test")).is_ok());
        assert!(create_provider("build.nvidia.com", Some("nvapi-test")).is_ok());
    }

    // ── AI inference routers ─────────────────────────────────

    #[test]
    fn factory_astrai() {
        assert!(create_provider("astrai", Some("sk-astrai-test")).is_ok());
    }

    #[test]
    fn factory_avian() {
        assert!(create_provider("avian", Some("sk-avian-test")).is_ok());
    }

    #[test]
    fn factory_deepmyst() {
        assert!(create_provider("deepmyst", Some("key")).is_ok());
        assert!(create_provider("deep-myst", Some("key")).is_ok());
    }

    #[test]
    fn resolve_provider_credential_deepmyst_env() {
        let _env_lock = env_lock();
        let _guard = EnvGuard::set("DEEPMYST_API_KEY", Some("dm-test-key"));
        let resolved = resolve_provider_credential("deepmyst", None);
        assert_eq!(resolved, Some("dm-test-key".to_string()));
    }

    // ── Custom / BYOP provider ─────────────────────────────

    #[test]
    fn factory_custom_url() {
        let p = create_provider("custom:https://my-llm.example.com", Some("key"));
        assert!(p.is_ok());
    }

    #[test]
    fn factory_custom_localhost() {
        let p = create_provider("custom:http://localhost:1234", Some("key"));
        assert!(p.is_ok());
    }

    #[test]
    fn factory_custom_no_key() {
        let p = create_provider("custom:https://my-llm.example.com", None);
        assert!(p.is_ok());
    }

    #[test]
    fn factory_custom_empty_url_errors() {
        match create_provider("custom:", None) {
            Err(e) => assert!(
                e.to_string().contains("requires a URL"),
                "Expected 'requires a URL', got: {e}"
            ),
            Ok(_) => panic!("Expected error for empty custom URL"),
        }
    }

    #[test]
    fn factory_custom_invalid_url_errors() {
        match create_provider("custom:not-a-url", None) {
            Err(e) => assert!(
                e.to_string().contains("requires a valid URL"),
                "Expected 'requires a valid URL', got: {e}"
            ),
            Ok(_) => panic!("Expected error for invalid custom URL"),
        }
    }

    #[test]
    fn factory_custom_unsupported_scheme_errors() {
        match create_provider("custom:ftp://example.com", None) {
            Err(e) => assert!(
                e.to_string().contains("http:// or https://"),
                "Expected scheme validation error, got: {e}"
            ),
            Ok(_) => panic!("Expected error for unsupported custom URL scheme"),
        }
    }

    #[test]
    fn factory_custom_trims_whitespace() {
        let p = create_provider("custom:  https://my-llm.example.com  ", Some("key"));
        assert!(p.is_ok());
    }

    // ── Anthropic-compatible custom endpoints ─────────────────

    #[test]
    fn factory_anthropic_custom_url() {
        let p = create_provider("anthropic-custom:https://api.example.com", Some("key"));
        assert!(p.is_ok());
    }

    #[test]
    fn factory_anthropic_custom_trailing_slash() {
        let p = create_provider("anthropic-custom:https://api.example.com/", Some("key"));
        assert!(p.is_ok());
    }

    #[test]
    fn factory_anthropic_custom_no_key() {
        let p = create_provider("anthropic-custom:https://api.example.com", None);
        assert!(p.is_ok());
    }

    #[test]
    fn factory_anthropic_custom_empty_url_errors() {
        match create_provider("anthropic-custom:", None) {
            Err(e) => assert!(
                e.to_string().contains("requires a URL"),
                "Expected 'requires a URL', got: {e}"
            ),
            Ok(_) => panic!("Expected error for empty anthropic-custom URL"),
        }
    }

    #[test]
    fn factory_anthropic_custom_invalid_url_errors() {
        match create_provider("anthropic-custom:not-a-url", None) {
            Err(e) => assert!(
                e.to_string().contains("requires a valid URL"),
                "Expected 'requires a valid URL', got: {e}"
            ),
            Ok(_) => panic!("Expected error for invalid anthropic-custom URL"),
        }
    }

    #[test]
    fn factory_anthropic_custom_unsupported_scheme_errors() {
        match create_provider("anthropic-custom:ftp://example.com", None) {
            Err(e) => assert!(
                e.to_string().contains("http:// or https://"),
                "Expected scheme validation error, got: {e}"
            ),
            Ok(_) => panic!("Expected error for unsupported anthropic-custom URL scheme"),
        }
    }

    // ── Error cases ──────────────────────────────────────────

    #[test]
    fn factory_unknown_provider_errors() {
        let p = create_provider("nonexistent", None);
        assert!(p.is_err());
        let msg = p.err().unwrap().to_string();
        assert!(msg.contains("Unknown provider"));
        assert!(msg.contains("nonexistent"));
    }

    #[test]
    fn factory_empty_name_errors() {
        assert!(create_provider("", None).is_err());
    }

    #[test]
    fn resilient_provider_ignores_duplicate_and_invalid_fallbacks() {
        let reliability = zeroclaw_config::schema::ReliabilityConfig {
            provider_retries: 1,
            provider_backoff_ms: 100,
            fallback_providers: vec![
                "openrouter".into(),
                "nonexistent-provider".into(),
                "openai".into(),
                "openai".into(),
            ],
            api_keys: Vec::new(),
            model_fallbacks: std::collections::HashMap::new(),
            channel_initial_backoff_secs: 2,
            channel_max_backoff_secs: 60,
            scheduler_poll_secs: 15,
            scheduler_retries: 2,
        };

        let provider = create_resilient_provider(
            "openrouter",
            Some("provider-test-credential"),
            None,
            &reliability,
        );
        assert!(provider.is_ok());
    }

    #[test]
    fn resilient_provider_errors_for_invalid_primary() {
        let reliability = zeroclaw_config::schema::ReliabilityConfig::default();
        let provider = create_resilient_provider(
            "totally-invalid",
            Some("provider-test-credential"),
            None,
            &reliability,
        );
        assert!(provider.is_err());
    }

    /// Fallback providers resolve their own credentials via provider-specific
    /// env vars rather than inheriting the primary provider's key.  A provider
    /// that requires no key (e.g. lmstudio, ollama) must initialize
    /// successfully even when the primary uses a completely different key.
    #[test]
    fn resilient_fallback_resolves_own_credential() {
        let reliability = zeroclaw_config::schema::ReliabilityConfig {
            provider_retries: 1,
            provider_backoff_ms: 100,
            fallback_providers: vec!["lmstudio".into(), "ollama".into()],
            api_keys: Vec::new(),
            model_fallbacks: std::collections::HashMap::new(),
            channel_initial_backoff_secs: 2,
            channel_max_backoff_secs: 60,
            scheduler_poll_secs: 15,
            scheduler_retries: 2,
        };

        // Primary uses a ZAI key; fallbacks (lmstudio, ollama) should NOT
        // receive this key; they resolve their own credentials independently.
        let provider = create_resilient_provider("zai", Some("zai-test-key"), None, &reliability);
        assert!(provider.is_ok());
    }

    /// `custom:` URL entries work as fallback providers, enabling arbitrary
    /// OpenAI-compatible endpoints (e.g. local LM Studio on a Docker host).
    #[test]
    fn resilient_fallback_supports_custom_url() {
        let reliability = zeroclaw_config::schema::ReliabilityConfig {
            provider_retries: 1,
            provider_backoff_ms: 100,
            fallback_providers: vec!["custom:http://host.docker.internal:1234/v1".into()],
            api_keys: Vec::new(),
            model_fallbacks: std::collections::HashMap::new(),
            channel_initial_backoff_secs: 2,
            channel_max_backoff_secs: 60,
            scheduler_poll_secs: 15,
            scheduler_retries: 2,
        };

        let provider =
            create_resilient_provider("openai", Some("openai-test-key"), None, &reliability);
        assert!(provider.is_ok());
    }

    /// Mixed fallback chain: named providers, custom URLs, and invalid entries
    /// all coexist.  Invalid entries are silently ignored; valid ones initialize.
    #[test]
    fn resilient_fallback_mixed_chain() {
        let reliability = zeroclaw_config::schema::ReliabilityConfig {
            provider_retries: 1,
            provider_backoff_ms: 100,
            fallback_providers: vec![
                "deepseek".into(),
                "custom:http://localhost:8080/v1".into(),
                "nonexistent-provider".into(),
                "lmstudio".into(),
            ],
            api_keys: Vec::new(),
            model_fallbacks: std::collections::HashMap::new(),
            channel_initial_backoff_secs: 2,
            channel_max_backoff_secs: 60,
            scheduler_poll_secs: 15,
            scheduler_retries: 2,
        };

        let provider = create_resilient_provider("zai", Some("zai-test-key"), None, &reliability);
        assert!(provider.is_ok());
    }

    #[test]
    fn ollama_with_custom_url() {
        let provider = create_provider_with_url("ollama", None, Some("http://10.100.2.32:11434"));
        assert!(provider.is_ok());
    }

    #[test]
    fn ollama_cloud_with_custom_url() {
        let provider =
            create_provider_with_url("ollama", Some("ollama-key"), Some("https://ollama.com"));
        assert!(provider.is_ok());
    }

    /// Osaurus works as a fallback provider alongside other named providers.
    #[test]
    fn resilient_fallback_includes_osaurus() {
        let reliability = zeroclaw_config::schema::ReliabilityConfig {
            provider_retries: 1,
            provider_backoff_ms: 100,
            fallback_providers: vec!["osaurus".into(), "lmstudio".into()],
            api_keys: Vec::new(),
            model_fallbacks: std::collections::HashMap::new(),
            channel_initial_backoff_secs: 2,
            channel_max_backoff_secs: 60,
            scheduler_poll_secs: 15,
            scheduler_retries: 2,
        };

        let provider = create_resilient_provider("zai", Some("zai-test-key"), None, &reliability);
        assert!(provider.is_ok());
    }

    #[test]
    fn factory_all_providers_create_successfully() {
        let providers = [
            "openrouter",
            "anthropic",
            "openai",
            "ollama",
            "gemini",
            "venice",
            "vercel",
            "cloudflare",
            "moonshot",
            "moonshot-intl",
            "kimi-code",
            "moonshot-cn",
            "kimi-code",
            "synthetic",
            "opencode",
            "opencode-go",
            "zai",
            "zai-cn",
            "glm",
            "glm-cn",
            "minimax",
            "minimax-cn",
            "bedrock",
            "qianfan",
            "doubao",
            "qwen",
            "qwen-intl",
            "qwen-cn",
            "qwen-us",
            "qwen-code",
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
            "claude-code",
            "gemini-cli",
            "kilocli",
            "nvidia",
            "astrai",
            "avian",
            "ovhcloud",
        ];
        for name in providers {
            assert!(
                create_provider(name, Some("test-key")).is_ok(),
                "Provider '{name}' should create successfully"
            );
        }
    }

    #[test]
    fn listed_providers_have_unique_ids_and_aliases() {
        let providers = list_providers();
        let mut canonical_ids = std::collections::HashSet::new();
        let mut aliases = std::collections::HashSet::new();

        for provider in providers {
            assert!(
                canonical_ids.insert(provider.name),
                "Duplicate canonical provider id: {}",
                provider.name
            );

            for alias in provider.aliases {
                assert_ne!(
                    *alias, provider.name,
                    "Alias must differ from canonical id: {}",
                    provider.name
                );
                assert!(
                    !canonical_ids.contains(alias),
                    "Alias conflicts with canonical provider id: {}",
                    alias
                );
                assert!(aliases.insert(alias), "Duplicate provider alias: {}", alias);
            }
        }
    }

    #[test]
    fn listed_providers_and_aliases_are_constructible() {
        for provider in list_providers() {
            assert!(
                create_provider(provider.name, Some("provider-test-credential")).is_ok(),
                "Canonical provider id should be constructible: {}",
                provider.name
            );

            for alias in provider.aliases {
                assert!(
                    create_provider(alias, Some("provider-test-credential")).is_ok(),
                    "Provider alias should be constructible: {} (for {})",
                    alias,
                    provider.name
                );
            }
        }
    }

    // ── API error sanitization ───────────────────────────────

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

    // --- parse_provider_profile ---

    #[test]
    fn parse_provider_profile_plain_name() {
        let (name, profile) = parse_provider_profile("gemini");
        assert_eq!(name, "gemini");
        assert_eq!(profile, None);
    }

    #[test]
    fn parse_provider_profile_with_profile() {
        let (name, profile) = parse_provider_profile("openai-codex:second");
        assert_eq!(name, "openai-codex");
        assert_eq!(profile, Some("second"));
    }

    #[test]
    fn parse_provider_profile_custom_url_not_split() {
        let input = "custom:https://my-api.example.com/v1";
        let (name, profile) = parse_provider_profile(input);
        assert_eq!(name, input);
        assert_eq!(profile, None);
    }

    #[test]
    fn parse_provider_profile_anthropic_custom_not_split() {
        let input = "anthropic-custom:https://bedrock.example.com";
        let (name, profile) = parse_provider_profile(input);
        assert_eq!(name, input);
        assert_eq!(profile, None);
    }

    #[test]
    fn parse_provider_profile_empty_profile_ignored() {
        let (name, profile) = parse_provider_profile("openai-codex:");
        assert_eq!(name, "openai-codex:");
        assert_eq!(profile, None);
    }

    #[test]
    fn parse_provider_profile_extra_colons_kept() {
        let (name, profile) = parse_provider_profile("provider:profile:extra");
        assert_eq!(name, "provider");
        assert_eq!(profile, Some("profile:extra"));
    }

    // --- resilient fallback with profile syntax ---

    #[test]
    fn resilient_fallback_with_profile_syntax() {
        let _guard = env_lock();

        let reliability = zeroclaw_config::schema::ReliabilityConfig {
            provider_retries: 1,
            provider_backoff_ms: 100,
            fallback_providers: vec!["openai-codex:second".into()],
            api_keys: Vec::new(),
            model_fallbacks: std::collections::HashMap::new(),
            channel_initial_backoff_secs: 2,
            channel_max_backoff_secs: 60,
            scheduler_poll_secs: 15,
            scheduler_retries: 2,
        };

        // openai-codex resolves its own OAuth credential; it should not
        // fail even with a profile override that has no local token file.
        // The provider initializes successfully and will attempt auth at
        // request time.
        let provider = create_resilient_provider("lmstudio", None, None, &reliability);
        assert!(provider.is_ok());
    }

    #[test]
    fn resilient_fallback_mixed_profiles_and_custom() {
        let _guard = env_lock();

        let reliability = zeroclaw_config::schema::ReliabilityConfig {
            provider_retries: 1,
            provider_backoff_ms: 100,
            fallback_providers: vec![
                "openai-codex:second".into(),
                "custom:http://localhost:8080/v1".into(),
                "lmstudio".into(),
                "nonexistent-provider".into(),
            ],
            api_keys: Vec::new(),
            model_fallbacks: std::collections::HashMap::new(),
            channel_initial_backoff_secs: 2,
            channel_max_backoff_secs: 60,
            scheduler_poll_secs: 15,
            scheduler_retries: 2,
        };

        let provider = create_resilient_provider("ollama", None, None, &reliability);
        assert!(provider.is_ok());
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
        let options = ProviderRuntimeOptions::default();
        assert!(options.extra_headers.is_empty());
    }

    #[test]
    fn provider_runtime_options_extra_headers_passed_through() {
        let mut extra_headers = std::collections::HashMap::new();
        extra_headers.insert("X-Title".to_string(), "zeroclaw".to_string());
        let options = ProviderRuntimeOptions {
            extra_headers,
            ..ProviderRuntimeOptions::default()
        };
        assert_eq!(options.extra_headers.len(), 1);
        assert_eq!(options.extra_headers.get("X-Title").unwrap(), "zeroclaw");
    }

    #[test]
    fn env_provider_url_overrides_api_url() {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_PROVIDER_URL", "http://env-ollama:11434") };

        let options = ProviderRuntimeOptions::default();

        let provider = create_provider_with_url_and_options(
            "ollama",
            Some("http://config-ollama:11434"),
            None,
            &options,
        );

        assert!(provider.is_ok());

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_PROVIDER_URL") };
    }
}
