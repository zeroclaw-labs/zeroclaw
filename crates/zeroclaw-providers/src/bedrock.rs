//! AWS Bedrock model_provider using the Converse API.
//!
//! Authentication: supports three methods:
//! - **Bearer token**: set `BEDROCK_API_KEY` env var (takes precedence).
//! - **SigV4 signing**: AWS AKSK (Access Key ID + Secret Access Key)
//!   via environment variables, `credential_process` in `~/.aws/config`,
//!   or EC2 IMDSv2. SigV4 signing is implemented manually using hmac/sha2
//!   crates — no AWS SDK dependency.

use crate::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    ModelProvider, ProviderCapabilities, TokenUsage, ToolCall as ProviderToolCall, ToolsPayload,
};
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use zeroclaw_api::tool::ToolSpec;

/// Hostname prefix for the Bedrock Runtime endpoint.
const ENDPOINT_PREFIX: &str = "bedrock-runtime";
/// SigV4 signing service name (AWS uses "bedrock", not "bedrock-runtime").
const SIGNING_SERVICE: &str = "bedrock";
const DEFAULT_REGION: &str = "us-east-1";

// ── Authentication ──────────────────────────────────────────────

/// Authentication method for Bedrock: either SigV4 (AKSK) or Bearer token.
enum BedrockAuth {
    SigV4(AwsCredentials),
    BearerToken(String),
}

// ── AWS Credentials ─────────────────────────────────────────────

/// Resolved AWS credentials for SigV4 signing.
#[derive(Clone)]
struct AwsCredentials {
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
    region: String,
    /// Credential expiry (from `credential_process` `Expiration` field).
    /// `None` means no known expiry — treat as long-lived.
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl AwsCredentials {
    /// Resolve credentials: first try environment variables, then EC2 IMDSv2.
    fn from_env() -> anyhow::Result<Self> {
        let access_key_id = env_required("AWS_ACCESS_KEY_ID")?;
        let secret_access_key = env_required("AWS_SECRET_ACCESS_KEY")?;

        let session_token = env_optional("AWS_SESSION_TOKEN");

        let region = env_optional("AWS_REGION")
            .or_else(|| env_optional("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|| DEFAULT_REGION.to_string());

        Ok(Self {
            access_key_id,
            secret_access_key,
            session_token,
            region,
            expires_at: None,
        })
    }

    /// Parse `~/.aws/config` (or `$AWS_CONFIG_FILE`) and return the
    /// `credential_process` command and optional `region` for the active profile.
    fn parse_aws_config(content: &str, profile: &str) -> Option<(String, Option<String>)> {
        let target = if profile == "default" {
            "[default]".to_string()
        } else {
            format!("[profile {profile}]")
        };

        let mut in_section = false;
        let mut cred_process = None;
        let mut region = None;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_section = trimmed == target;
                continue;
            }
            if !in_section || trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }
            if let Some((key, value)) = trimmed.split_once('=') {
                match key.trim() {
                    "credential_process" => cred_process = Some(value.trim().to_string()),
                    "region" => region = Some(value.trim().to_string()),
                    _ => {}
                }
            }
        }
        cred_process.map(|cmd| (cmd, region))
    }

    /// Resolve credentials via `credential_process` in `~/.aws/config`.
    fn from_credential_process() -> anyhow::Result<Self> {
        let config_path = std::env::var("AWS_CONFIG_FILE").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "~".to_string());
            format!("{home}/.aws/config")
        });
        let content = std::fs::read_to_string(&config_path)
            .map_err(|e| anyhow::anyhow!("Cannot read {config_path}: {e}"))?;
        let profile = std::env::var("AWS_PROFILE").unwrap_or_else(|_| "default".to_string());
        let (cmd, config_region) = Self::parse_aws_config(&content, &profile)
            .ok_or_else(|| anyhow::anyhow!("No credential_process in [{profile}]"))?;

        let output = std::process::Command::new("sh")
            .args(["-c", &cmd])
            .output()
            .map_err(|e| anyhow::anyhow!("Failed to run credential_process: {e}"))?;
        anyhow::ensure!(
            output.status.success(),
            "credential_process exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );

        let json: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| anyhow::anyhow!("credential_process output is not valid JSON: {e}"))?;

        let access_key_id = json["AccessKeyId"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing AccessKeyId in credential_process output"))?
            .to_string();
        let secret_access_key = json["SecretAccessKey"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing SecretAccessKey in credential_process output"))?
            .to_string();
        let session_token = json["SessionToken"].as_str().map(|s| s.to_string());

        let expires_at = json["Expiration"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        let region = env_optional("AWS_REGION")
            .or_else(|| env_optional("AWS_DEFAULT_REGION"))
            .or(config_region)
            .unwrap_or_else(|| DEFAULT_REGION.to_string());

        tracing::debug!("Loaded AWS credentials via credential_process");

        Ok(Self {
            access_key_id,
            secret_access_key,
            session_token,
            region,
            expires_at,
        })
    }

    /// Fetch credentials from EC2 IMDSv2 instance metadata service.
    async fn from_imds() -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()?;

        // Step 1: get IMDSv2 token
        let token = client
            .put("http://169.254.169.254/latest/api/token")
            .header("X-aws-ec2-metadata-token-ttl-seconds", "21600")
            .send()
            .await?
            .text()
            .await?;

        // Step 2: get IAM role name
        let role = client
            .get("http://169.254.169.254/latest/meta-data/iam/security-credentials/")
            .header("X-aws-ec2-metadata-token", &token)
            .send()
            .await?
            .text()
            .await?;
        let role = role.trim().to_string();
        anyhow::ensure!(!role.is_empty(), "No IAM role attached to this instance");

        // Step 3: get credentials for that role
        let creds_url = format!(
            "http://169.254.169.254/latest/meta-data/iam/security-credentials/{}",
            role
        );
        let creds_json: serde_json::Value = client
            .get(&creds_url)
            .header("X-aws-ec2-metadata-token", &token)
            .send()
            .await?
            .json()
            .await?;

        let access_key_id = creds_json["AccessKeyId"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing AccessKeyId in IMDS response"))?
            .to_string();
        let secret_access_key = creds_json["SecretAccessKey"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing SecretAccessKey in IMDS response"))?
            .to_string();
        let session_token = creds_json["Token"].as_str().map(|s| s.to_string());

        // Step 4: get region from instance identity document
        let region = match client
            .get("http://169.254.169.254/latest/meta-data/placement/region")
            .header("X-aws-ec2-metadata-token", &token)
            .send()
            .await
        {
            Ok(resp) => resp.text().await.unwrap_or_default(),
            Err(_) => String::new(),
        };
        let region = if region.trim().is_empty() {
            env_optional("AWS_REGION")
                .or_else(|| env_optional("AWS_DEFAULT_REGION"))
                .unwrap_or_else(|| DEFAULT_REGION.to_string())
        } else {
            region.trim().to_string()
        };

        tracing::info!(
            "Loaded AWS credentials from EC2 instance metadata (role: {})",
            role
        );

        Ok(Self {
            access_key_id,
            secret_access_key,
            session_token,
            region,
            expires_at: None,
        })
    }

    /// Resolve credentials: env vars first, then credential_process, then EC2 IMDS.
    async fn resolve() -> anyhow::Result<Self> {
        if let Ok(creds) = Self::from_env() {
            return Ok(creds);
        }
        if let Ok(creds) = Self::from_credential_process() {
            return Ok(creds);
        }
        Self::from_imds().await
    }

    fn host(&self) -> String {
        format!("{ENDPOINT_PREFIX}.{}.amazonaws.com", self.region)
    }

    /// Returns `true` if credentials have a known expiry that has passed
    /// (with 60s skew to allow for clock drift and network latency).
    fn is_expired(&self) -> bool {
        match self.expires_at {
            Some(exp) => chrono::Utc::now() >= exp - chrono::Duration::seconds(60),
            None => false,
        }
    }
}

fn env_required(name: &str) -> anyhow::Result<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Environment variable {name} is required for Bedrock"))
}

fn env_optional(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

// ── AWS SigV4 Signing ───────────────────────────────────────────

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC can take key of any size");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// Derive the SigV4 signing key via HMAC chain.
fn derive_signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

/// Build the SigV4 `Authorization` header value.
///
/// `headers` must be sorted by lowercase header name.
fn build_authorization_header(
    credentials: &AwsCredentials,
    method: &str,
    canonical_uri: &str,
    query_string: &str,
    headers: &[(String, String)],
    payload: &[u8],
    timestamp: &chrono::DateTime<chrono::Utc>,
) -> String {
    let date_stamp = timestamp.format("%Y%m%d").to_string();
    let amz_date = timestamp.format("%Y%m%dT%H%M%SZ").to_string();

    let mut canonical_headers = String::new();
    for (k, v) in headers {
        canonical_headers.push_str(k);
        canonical_headers.push(':');
        canonical_headers.push_str(v);
        canonical_headers.push('\n');
    }

    let signed_headers: String = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");

    let payload_hash = sha256_hex(payload);

    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{query_string}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );

    let credential_scope = format!(
        "{date_stamp}/{}/{SIGNING_SERVICE}/aws4_request",
        credentials.region
    );

    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let signing_key = derive_signing_key(
        &credentials.secret_access_key,
        &date_stamp,
        &credentials.region,
        SIGNING_SERVICE,
    );

    let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes()));

    format!(
        "AWS4-HMAC-SHA256 Credential={}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}",
        credentials.access_key_id
    )
}

// ── Converse API Types (Request) ────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConverseRequest {
    messages: Vec<ConverseMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<Vec<SystemBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inference_config: Option<InferenceConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_config: Option<ToolConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ConverseMessage {
    role: String,
    content: Vec<ContentBlock>,
}

/// Content blocks use Bedrock's union style:
/// `{"text": "..."}`, `{"toolUse": {...}}`, `{"toolResult": {...}}`, `{"cachePoint": {...}}`.
///
/// Note: `text` is a simple string value, not a nested object. `toolUse` and `toolResult`
/// are nested objects. We use `#[serde(untagged)]` with manual struct wrappers to
/// match this mixed format.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum ContentBlock {
    Text(TextBlock),
    ToolUse(ToolUseWrapper),
    ToolResult(ToolResultWrapper),
    CachePointBlock(CachePointWrapper),
    Image(ImageWrapper),
}

#[derive(Debug, Serialize, Deserialize)]
struct ImageWrapper {
    image: ImageBlock,
}

#[derive(Debug, Serialize, Deserialize)]
struct ImageBlock {
    format: String,
    source: ImageSource,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImageSource {
    bytes: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct TextBlock {
    text: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolUseWrapper {
    tool_use: ToolUseBlock,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolUseBlock {
    tool_use_id: String,
    name: String,
    input: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolResultWrapper {
    tool_result: ToolResultBlock,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolResultBlock {
    tool_use_id: String,
    content: Vec<ToolResultContent>,
    status: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachePointWrapper {
    cache_point: CachePoint,
}

#[derive(Debug, Serialize, Deserialize)]
struct ToolResultContent {
    text: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachePoint {
    #[serde(rename = "type")]
    cache_type: String,
}

impl CachePoint {
    fn default_cache() -> Self {
        Self {
            cache_type: "default".to_string(),
        }
    }
}

/// System prompt blocks: either `{"text": "..."}` or `{"cachePoint": {...}}`.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum SystemBlock {
    Text(TextBlock),
    CachePoint(CachePointWrapper),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InferenceConfig {
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
}

/// Some Bedrock models (the Claude opus-4-7 family) reject `temperature` in
/// `inferenceConfig` with a 400 "temperature is deprecated for this model".
/// Substring match covers region/profile prefixes (e.g. `us.anthropic.…`)
/// and version suffixes (e.g. `-v1:0`).
fn bedrock_model_omits_temperature(model: &str) -> bool {
    model.contains("claude-opus-4-7")
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolConfig {
    tools: Vec<ToolDefinition>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolDefinition {
    tool_spec: ToolSpecDef,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolSpecDef {
    name: String,
    description: String,
    input_schema: InputSchema,
}

#[derive(Debug, Serialize)]
struct InputSchema {
    json: serde_json::Value,
}

// ── Converse API Types (Response) ───────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConverseResponse {
    #[serde(default)]
    output: Option<ConverseOutput>,
    #[serde(default)]
    #[allow(dead_code)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<BedrockUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BedrockUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ConverseOutput {
    #[serde(default)]
    message: Option<ConverseOutputMessage>,
}

#[derive(Debug, Deserialize)]
struct ConverseOutputMessage {
    #[allow(dead_code)]
    role: String,
    content: Vec<ResponseContentBlock>,
}

/// Response content blocks from the Converse API.
///
/// Uses `#[serde(untagged)]` to match Bedrock's union format where `text` is a
/// simple string value and `toolUse` is a nested object. Unknown block types
/// (e.g. `reasoningContent`, `guardContent`) are captured as `Other` to prevent
/// deserialization failures.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ResponseContentBlock {
    ToolUse(ResponseToolUseWrapper),
    Text(TextBlock),
    Other(#[allow(dead_code)] serde_json::Value),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResponseToolUseWrapper {
    tool_use: ToolUseBlock,
}

// ── BedrockModelProvider ─────────────────────────────────────────────

pub struct BedrockModelProvider {
    auth: Option<BedrockAuth>,
    max_tokens: u32,
    /// Cached SigV4 credentials from `credential_process` (with expiry).
    cred_cache: Mutex<Option<AwsCredentials>>,
}

impl Default for BedrockModelProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl BedrockModelProvider {
    pub fn new() -> Self {
        // Bearer token takes precedence over SigV4 credentials.
        if let Some(token) = env_optional("BEDROCK_API_KEY") {
            return Self {
                auth: Some(BedrockAuth::BearerToken(token)),
                max_tokens: zeroclaw_api::model_provider::BASELINE_MAX_TOKENS,
                cred_cache: Mutex::new(None),
            };
        }
        Self {
            auth: AwsCredentials::from_env()
                .or_else(|_| AwsCredentials::from_credential_process())
                .ok()
                .map(BedrockAuth::SigV4),
            max_tokens: zeroclaw_api::model_provider::BASELINE_MAX_TOKENS,
            cred_cache: Mutex::new(None),
        }
    }

    pub async fn new_async() -> Self {
        // Bearer token takes precedence over SigV4 credentials.
        if let Some(token) = env_optional("BEDROCK_API_KEY") {
            return Self {
                auth: Some(BedrockAuth::BearerToken(token)),
                max_tokens: zeroclaw_api::model_provider::BASELINE_MAX_TOKENS,
                cred_cache: Mutex::new(None),
            };
        }
        let auth = AwsCredentials::resolve().await.ok().map(BedrockAuth::SigV4);
        Self {
            auth,
            max_tokens: zeroclaw_api::model_provider::BASELINE_MAX_TOKENS,
            cred_cache: Mutex::new(None),
        }
    }

    /// Create a model_provider using a Bearer token for authentication.
    pub fn with_bearer_token(token: &str) -> Self {
        Self {
            auth: Some(BedrockAuth::BearerToken(token.to_string())),
            max_tokens: zeroclaw_api::model_provider::BASELINE_MAX_TOKENS,
            cred_cache: Mutex::new(None),
        }
    }

    /// Override the maximum output tokens for API requests.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    fn http_client(&self) -> Client {
        zeroclaw_config::schema::build_runtime_proxy_client_with_timeouts(
            "model_provider.bedrock",
            120,
            10,
        )
    }

    /// Percent-encode the model ID for URL path: only encode `:` to `%3A`.
    /// Colons in model IDs (e.g. `v1:0`) must be encoded because `reqwest::Url`
    /// may misparse them. Dots, hyphens, and alphanumerics are safe.
    fn encode_model_path(model_id: &str) -> String {
        model_id.replace(':', "%3A")
    }

    /// Resolve the AWS region from environment variables.
    fn resolve_region() -> String {
        env_optional("AWS_REGION")
            .or_else(|| env_optional("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|| DEFAULT_REGION.to_string())
    }

    /// Build the actual request URL. Uses raw model ID (reqwest sends colons as-is).
    fn endpoint_url(region: &str, model_id: &str) -> String {
        format!("https://{ENDPOINT_PREFIX}.{region}.amazonaws.com/model/{model_id}/converse")
    }

    /// Build the canonical URI for SigV4 signing. Must URI-encode the path
    /// per SigV4 spec: colons become `%3A`. AWS verifies the signature against
    /// the encoded form even though the wire request uses raw colons.
    fn canonical_uri(model_id: &str) -> String {
        let encoded = Self::encode_model_path(model_id);
        format!("/model/{encoded}/converse")
    }

    /// Check the credential cache for unexpired credentials.
    fn cached_credentials(&self) -> Option<AwsCredentials> {
        let cache = self.cred_cache.lock().ok()?;
        let creds = cache.as_ref()?;
        if creds.is_expired() {
            return None;
        }
        Some(creds.clone())
    }

    /// Store credentials in the cache.
    fn cache_credentials(&self, creds: &AwsCredentials) {
        if let Ok(mut cache) = self.cred_cache.lock() {
            *cache = Some(creds.clone());
        }
    }

    /// Resolve auth: use cached if available, otherwise try env vars then IMDS.
    async fn resolve_auth(&self) -> anyhow::Result<BedrockAuth> {
        // If we already have auth cached, re-resolve from the same source.
        if let Some(ref auth) = self.auth {
            match auth {
                BedrockAuth::BearerToken(token) => {
                    return Ok(BedrockAuth::BearerToken(token.clone()));
                }
                BedrockAuth::SigV4(_) => {
                    if let Some(creds) = self.cached_credentials() {
                        return Ok(BedrockAuth::SigV4(creds));
                    }
                }
            }
        }
        // Check Bearer token first.
        if let Some(token) = env_optional("BEDROCK_API_KEY") {
            return Ok(BedrockAuth::BearerToken(token));
        }
        // Fall back to SigV4.
        if let Ok(creds) = AwsCredentials::from_env() {
            return Ok(BedrockAuth::SigV4(creds));
        }
        if let Ok(creds) = AwsCredentials::from_credential_process() {
            self.cache_credentials(&creds);
            return Ok(BedrockAuth::SigV4(creds));
        }
        Ok(BedrockAuth::SigV4(AwsCredentials::from_imds().await?))
    }

    // ── Cache heuristics (same thresholds as AnthropicModelProvider) ──

    /// Cache system prompts larger than ~1024 tokens (3KB of text).
    fn should_cache_system(text: &str) -> bool {
        text.len() > 3072
    }

    /// Cache conversations with more than 4 messages (excluding system).
    fn should_cache_conversation(messages: &[ChatMessage]) -> bool {
        messages.iter().filter(|m| m.role != "system").count() > 4
    }

    // ── Message conversion ──────────────────────────────────────

    fn convert_messages(
        messages: &[ChatMessage],
    ) -> (Option<Vec<SystemBlock>>, Vec<ConverseMessage>) {
        let mut system_blocks = Vec::new();
        let mut converse_messages = Vec::new();

        for msg in messages {
            match msg.role.as_str() {
                "system" => {
                    if system_blocks.is_empty() {
                        system_blocks.push(SystemBlock::Text(TextBlock {
                            text: msg.content.clone(),
                        }));
                    }
                }
                "assistant" => {
                    if let Some(blocks) = Self::parse_assistant_tool_call_message(&msg.content) {
                        converse_messages.push(ConverseMessage {
                            role: "assistant".to_string(),
                            content: blocks,
                        });
                    } else {
                        // Guard: never send an empty text block to Bedrock.
                        // This can happen when a daemon restart interrupts a
                        // streaming response, leaving a partially-persisted
                        // assistant message with empty content.
                        let text = if msg.content.trim().is_empty() {
                            "(empty response)".to_string()
                        } else {
                            msg.content.clone()
                        };
                        converse_messages.push(ConverseMessage {
                            role: "assistant".to_string(),
                            content: vec![ContentBlock::Text(TextBlock { text })],
                        });
                    }
                }
                "tool" => {
                    let tool_result_msg = Self::parse_tool_result_message(&msg.content)
                        .unwrap_or_else(|| {
                            // Fallback: always emit a toolResult block so the
                            // Bedrock API contract (every toolUse needs a matching
                            // toolResult) is never violated.
                            let tool_use_id = Self::extract_tool_call_id(&msg.content)
                                .or_else(|| Self::last_pending_tool_use_id(&converse_messages))
                                .unwrap_or_else(|| "unknown".to_string());

                            tracing::warn!(
                                "Failed to parse tool result message, creating error \
                                 toolResult for tool_use_id={}",
                                tool_use_id
                            );

                            ConverseMessage {
                                role: "user".to_string(),
                                content: vec![ContentBlock::ToolResult(ToolResultWrapper {
                                    tool_result: ToolResultBlock {
                                        tool_use_id,
                                        content: vec![ToolResultContent {
                                            text: msg.content.clone(),
                                        }],
                                        status: "error".to_string(),
                                    },
                                })],
                            }
                        });

                    // Merge consecutive tool results into a single user message.
                    // Bedrock requires all toolResult blocks for a multi-tool-call
                    // turn to appear in one user message.
                    if let Some(last) = converse_messages.last_mut()
                        && last.role == "user"
                        && last
                            .content
                            .iter()
                            .all(|b| matches!(b, ContentBlock::ToolResult(_)))
                    {
                        last.content.extend(tool_result_msg.content);
                        continue;
                    }
                    converse_messages.push(tool_result_msg);
                }
                _ => {
                    let content_blocks = Self::parse_user_content_blocks(&msg.content);
                    converse_messages.push(ConverseMessage {
                        role: "user".to_string(),
                        content: content_blocks,
                    });
                }
            }
        }

        let system = if system_blocks.is_empty() {
            None
        } else {
            Some(system_blocks)
        };
        (system, converse_messages)
    }

    /// Remove empty text ContentBlocks from converse messages.
    ///
    /// Bedrock rejects requests where a ContentBlock has a blank `text` field
    /// with: "The text field in the ContentBlock object is blank". This can
    /// occur when a daemon restart interrupts a streaming response, leaving a
    /// partially-persisted message with empty content, or when bot/attachment-
    /// only messages produce empty text blocks.
    fn sanitize_empty_content_blocks(messages: &mut [ConverseMessage]) {
        for msg in messages.iter_mut() {
            msg.content.retain(|block| match block {
                ContentBlock::Text(tb) => !tb.text.trim().is_empty(),
                _ => true,
            });
            if msg.content.is_empty() {
                msg.content.push(ContentBlock::Text(TextBlock {
                    text: "(empty)".to_string(),
                }));
            }
        }
    }

    /// Try to extract a tool_call_id from partially-valid JSON content.
    fn extract_tool_call_id(content: &str) -> Option<String> {
        let value = serde_json::from_str::<serde_json::Value>(content).ok()?;
        value
            .get("tool_call_id")
            .or_else(|| value.get("tool_use_id"))
            .or_else(|| value.get("toolUseId"))
            .and_then(serde_json::Value::as_str)
            .map(String::from)
    }

    /// Find the first unmatched tool_use_id from the last assistant message.
    ///
    /// When a tool result can't be parsed at all (not even the ID), we fall
    /// back to matching it against the preceding assistant turn's toolUse
    /// blocks that don't yet have a corresponding toolResult.
    fn last_pending_tool_use_id(converse_messages: &[ConverseMessage]) -> Option<String> {
        let last_assistant = converse_messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant")?;

        let tool_use_ids: Vec<&str> = last_assistant
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse(wrapper) => Some(wrapper.tool_use.tool_use_id.as_str()),
                _ => None,
            })
            .collect();

        let answered_ids: Vec<&str> = converse_messages
            .iter()
            .rev()
            .take_while(|m| m.role == "user")
            .flat_map(|m| m.content.iter())
            .filter_map(|b| match b {
                ContentBlock::ToolResult(wrapper) => Some(wrapper.tool_result.tool_use_id.as_str()),
                _ => None,
            })
            .collect();

        tool_use_ids
            .into_iter()
            .find(|id| !answered_ids.contains(id))
            .map(String::from)
    }

    /// Parse user message content, extracting [IMAGE:data:...] markers into image blocks.
    fn parse_user_content_blocks(content: &str) -> Vec<ContentBlock> {
        let mut blocks: Vec<ContentBlock> = Vec::new();
        let mut remaining = content;
        let has_image = content.contains("[IMAGE:");
        tracing::info!(
            "parse_user_content_blocks called, len={}, has_image={}",
            content.len(),
            has_image
        );

        while let Some(start) = remaining.find("[IMAGE:") {
            // Add any text before the marker
            let text_before = &remaining[..start];
            if !text_before.trim().is_empty() {
                blocks.push(ContentBlock::Text(TextBlock {
                    text: text_before.to_string(),
                }));
            }

            let after = &remaining[start + 7..]; // skip "[IMAGE:"
            if let Some(end) = after.find(']') {
                let src = &after[..end];
                remaining = &after[end + 1..];

                // Only handle data URIs (base64 encoded images)
                if let Some(rest) = src.strip_prefix("data:")
                    && let Some(semi) = rest.find(';')
                {
                    let mime = &rest[..semi];
                    let after_semi = &rest[semi + 1..];
                    if let Some(b64) = after_semi.strip_prefix("base64,") {
                        let format = match mime {
                            "image/png" => "png",
                            "image/gif" => "gif",
                            "image/webp" => "webp",
                            _ => "jpeg",
                        };
                        blocks.push(ContentBlock::Image(ImageWrapper {
                            image: ImageBlock {
                                format: format.to_string(),
                                source: ImageSource {
                                    bytes: b64.to_string(),
                                },
                            },
                        }));
                        continue;
                    }
                }
                // Non-data-uri image: just include as text reference
                blocks.push(ContentBlock::Text(TextBlock {
                    text: format!("[image: {}]", src),
                }));
            } else {
                // No closing bracket, treat rest as text
                blocks.push(ContentBlock::Text(TextBlock {
                    text: remaining.to_string(),
                }));
                break;
            }
        }

        // Add any remaining text
        if !remaining.trim().is_empty() {
            blocks.push(ContentBlock::Text(TextBlock {
                text: remaining.to_string(),
            }));
        }

        if blocks.is_empty() {
            let fallback = if content.trim().is_empty() {
                "(empty)".to_string()
            } else {
                content.to_string()
            };
            blocks.push(ContentBlock::Text(TextBlock { text: fallback }));
        }

        blocks
    }

    /// Parse assistant message containing structured tool calls.
    fn parse_assistant_tool_call_message(content: &str) -> Option<Vec<ContentBlock>> {
        let value = serde_json::from_str::<serde_json::Value>(content).ok()?;
        let tool_calls = value
            .get("tool_calls")
            .and_then(|v| serde_json::from_value::<Vec<ProviderToolCall>>(v.clone()).ok())?;

        let mut blocks = Vec::new();
        if let Some(text) = value
            .get("content")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            blocks.push(ContentBlock::Text(TextBlock {
                text: text.to_string(),
            }));
        }
        for call in tool_calls {
            let input = serde_json::from_str::<serde_json::Value>(&call.arguments)
                .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
            blocks.push(ContentBlock::ToolUse(ToolUseWrapper {
                tool_use: ToolUseBlock {
                    tool_use_id: call.id,
                    name: call.name,
                    input,
                },
            }));
        }
        Some(blocks)
    }

    /// Parse tool result message into a user message with ToolResult block.
    fn parse_tool_result_message(content: &str) -> Option<ConverseMessage> {
        let value = serde_json::from_str::<serde_json::Value>(content).ok()?;
        let tool_use_id = value
            .get("tool_call_id")
            .or_else(|| value.get("tool_use_id"))
            .or_else(|| value.get("toolUseId"))
            .and_then(serde_json::Value::as_str)?
            .to_string();
        let result = value
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        Some(ConverseMessage {
            role: "user".to_string(),
            content: vec![ContentBlock::ToolResult(ToolResultWrapper {
                tool_result: ToolResultBlock {
                    tool_use_id,
                    content: vec![ToolResultContent { text: result }],
                    status: "success".to_string(),
                },
            })],
        })
    }

    // ── Tool conversion ─────────────────────────────────────────

    fn convert_tools_to_converse(tools: Option<&[ToolSpec]>) -> Option<ToolConfig> {
        let items = tools?;
        if items.is_empty() {
            return None;
        }
        let tool_defs: Vec<ToolDefinition> = items
            .iter()
            .map(|tool| ToolDefinition {
                tool_spec: ToolSpecDef {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    input_schema: InputSchema {
                        json: tool.parameters.clone(),
                    },
                },
            })
            .collect();
        Some(ToolConfig { tools: tool_defs })
    }

    // ── Response parsing ────────────────────────────────────────

    fn parse_converse_response(response: ConverseResponse) -> ProviderChatResponse {
        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        let usage = response.usage.map(|u| TokenUsage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cached_input_tokens: None,
        });

        if let Some(output) = response.output
            && let Some(message) = output.message
        {
            for block in message.content {
                match block {
                    ResponseContentBlock::Text(tb) => {
                        let trimmed = tb.text.trim().to_string();
                        if !trimmed.is_empty() {
                            text_parts.push(trimmed);
                        }
                    }
                    ResponseContentBlock::ToolUse(wrapper) => {
                        if !wrapper.tool_use.name.is_empty() {
                            tool_calls.push(ProviderToolCall {
                                id: wrapper.tool_use.tool_use_id,
                                name: wrapper.tool_use.name,
                                arguments: wrapper.tool_use.input.to_string(),
                                extra_content: None,
                            });
                        }
                    }
                    ResponseContentBlock::Other(_) => {}
                }
            }
        }

        ProviderChatResponse {
            text: if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.join("\n"))
            },
            tool_calls,
            usage,
            reasoning_content: None,
        }
    }

    // ── HTTP request ────────────────────────────────────────────

    async fn send_converse_request(
        &self,
        auth: &BedrockAuth,
        model: &str,
        request_body: &ConverseRequest,
    ) -> anyhow::Result<ConverseResponse> {
        let payload = serde_json::to_vec(request_body)?;

        // Debug: log image blocks in payload (truncated)
        if let Ok(debug_val) = serde_json::from_slice::<serde_json::Value>(&payload)
            && let Some(msgs) = debug_val.get("messages").and_then(|m| m.as_array())
        {
            for msg in msgs {
                if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
                    for block in content {
                        if block.get("image").is_some() {
                            let mut b = block.clone();
                            if let Some(img) = b.get_mut("image")
                                && let Some(src) = img.get_mut("source")
                                && let Some(bytes) = src.get_mut("bytes")
                                && let Some(s) = bytes.as_str()
                            {
                                *bytes = serde_json::json!(format!("<base64 {} chars>", s.len()));
                            }
                            tracing::info!(
                                "Bedrock image block: {}",
                                serde_json::to_string(&b).unwrap_or_default()
                            );
                        }
                    }
                }
            }
        }

        let response: reqwest::Response = match auth {
            BedrockAuth::BearerToken(token) => {
                let region = Self::resolve_region();
                let url = Self::endpoint_url(&region, model);

                self.http_client()
                    .post(&url)
                    .header("content-type", "application/json")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(payload)
                    .send()
                    .await?
            }
            BedrockAuth::SigV4(credentials) => {
                let url = Self::endpoint_url(&credentials.region, model);
                let canonical_uri = Self::canonical_uri(model);
                let now = chrono::Utc::now();
                let host = credentials.host();
                let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();

                let mut headers_to_sign = vec![
                    ("content-type".to_string(), "application/json".to_string()),
                    ("host".to_string(), host),
                    ("x-amz-date".to_string(), amz_date.clone()),
                ];
                if let Some(ref session_token) = credentials.session_token {
                    headers_to_sign
                        .push(("x-amz-security-token".to_string(), session_token.clone()));
                }
                headers_to_sign.sort_by(|a, b| a.0.cmp(&b.0));

                let authorization = build_authorization_header(
                    credentials,
                    "POST",
                    &canonical_uri,
                    "",
                    &headers_to_sign,
                    &payload,
                    &now,
                );

                let mut request = self
                    .http_client()
                    .post(&url)
                    .header("content-type", "application/json")
                    .header("x-amz-date", &amz_date)
                    .header("authorization", &authorization);

                if let Some(ref session_token) = credentials.session_token {
                    request = request.header("x-amz-security-token", session_token);
                }

                request.body(payload).send().await?
            }
        };

        if !response.status().is_success() {
            return Err(super::api_error("Bedrock", response).await);
        }

        let converse_response: ConverseResponse = response.json().await?;
        Ok(converse_response)
    }
}

// ── ModelProvider trait implementation ───────────────────────────────

#[async_trait]
impl ModelProvider for BedrockModelProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: true,
            vision: true,
            prompt_caching: false,
        }
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    fn convert_tools(&self, tools: &[ToolSpec]) -> ToolsPayload {
        let tool_values: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "toolSpec": {
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": { "json": t.parameters }
                    }
                })
            })
            .collect();
        ToolsPayload::Anthropic { tools: tool_values }
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let temperature = temperature.unwrap_or(self.default_temperature());
        let auth = self.resolve_auth().await?;

        let system = system_prompt.map(|text| {
            let mut blocks = vec![SystemBlock::Text(TextBlock {
                text: text.to_string(),
            })];
            if Self::should_cache_system(text) {
                blocks.push(SystemBlock::CachePoint(CachePointWrapper {
                    cache_point: CachePoint::default_cache(),
                }));
            }
            blocks
        });

        let mut messages = vec![ConverseMessage {
            role: "user".to_string(),
            content: Self::parse_user_content_blocks(message),
        }];
        Self::sanitize_empty_content_blocks(&mut messages);

        let request = ConverseRequest {
            system,
            messages,
            inference_config: Some(InferenceConfig {
                max_tokens: self.max_tokens,
                temperature: if bedrock_model_omits_temperature(model) {
                    None
                } else {
                    Some(temperature)
                },
            }),
            tool_config: None,
        };

        let response = self.send_converse_request(&auth, model, &request).await?;

        Self::parse_converse_response(response)
            .text
            .ok_or_else(|| anyhow::anyhow!("No response from Bedrock"))
    }

    async fn chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ProviderChatResponse> {
        let temperature = temperature.unwrap_or(self.default_temperature());
        let auth = self.resolve_auth().await?;

        let (system_blocks, mut converse_messages) = Self::convert_messages(request.messages);

        // Strip empty text ContentBlocks that would cause Bedrock 400 errors.
        Self::sanitize_empty_content_blocks(&mut converse_messages);

        // Apply cachePoint to system if large.
        let system = system_blocks.map(|mut blocks| {
            let has_large_system = blocks
                .iter()
                .any(|b| matches!(b, SystemBlock::Text(tb) if Self::should_cache_system(&tb.text)));
            if has_large_system {
                blocks.push(SystemBlock::CachePoint(CachePointWrapper {
                    cache_point: CachePoint::default_cache(),
                }));
            }
            blocks
        });

        // Apply cachePoint to last message if conversation is long.
        if Self::should_cache_conversation(request.messages)
            && let Some(last_msg) = converse_messages.last_mut()
        {
            last_msg
                .content
                .push(ContentBlock::CachePointBlock(CachePointWrapper {
                    cache_point: CachePoint::default_cache(),
                }));
        }

        let tool_config = Self::convert_tools_to_converse(request.tools);

        let converse_request = ConverseRequest {
            system,
            messages: converse_messages,
            inference_config: Some(InferenceConfig {
                max_tokens: self.max_tokens,
                temperature: if bedrock_model_omits_temperature(model) {
                    None
                } else {
                    Some(temperature)
                },
            }),
            tool_config,
        };

        let response = self
            .send_converse_request(&auth, model, &converse_request)
            .await?;

        Ok(Self::parse_converse_response(response))
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        let region = match self.auth {
            Some(BedrockAuth::SigV4(ref creds)) => creds.region.clone(),
            Some(BedrockAuth::BearerToken(_)) => Self::resolve_region(),
            None => return Ok(()),
        };
        let url = format!("https://{ENDPOINT_PREFIX}.{region}.amazonaws.com/");
        let _ = self.http_client().get(&url).send().await;
        Ok(())
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{EnvGuard, env_lock};
    use crate::traits::ChatMessage;

    // ── SigV4 signing tests ─────────────────────────────────────

    #[test]
    fn sha256_hex_empty_string() {
        // Known SHA-256 of empty input
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_hex_known_input() {
        // SHA-256 of "hello"
        assert_eq!(
            sha256_hex(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    /// AWS documentation example key for SigV4 test vectors (not a real credential).
    const TEST_VECTOR_SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

    #[test]
    fn hmac_sha256_known_input() {
        let test_key: &[u8] = b"key";
        let result = hmac_sha256(test_key, b"message");
        assert_eq!(
            hex::encode(&result),
            "6e9ef29b75fffc5b7abae527d58fdadb2fe42e7219011976917343065f58ed4a"
        );
    }

    #[test]
    fn derive_signing_key_structure() {
        // Verify the key derivation produces a 32-byte key (SHA-256 output).
        let key = derive_signing_key(TEST_VECTOR_SECRET, "20150830", "us-east-1", "iam");
        assert_eq!(key.len(), 32);
    }

    #[test]
    fn derive_signing_key_known_test_vector() {
        // AWS SigV4 test vector from documentation.
        let key = derive_signing_key(TEST_VECTOR_SECRET, "20150830", "us-east-1", "iam");
        assert_eq!(
            hex::encode(&key),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
        );
    }

    #[test]
    fn build_authorization_header_format() {
        let credentials = AwsCredentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            session_token: None,
            region: "us-east-1".to_string(),
            expires_at: None,
        };

        let timestamp = chrono::DateTime::parse_from_rfc3339("2024-01-15T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        let headers = vec![
            ("content-type".to_string(), "application/json".to_string()),
            (
                "host".to_string(),
                "bedrock-runtime.us-east-1.amazonaws.com".to_string(),
            ),
            ("x-amz-date".to_string(), "20240115T120000Z".to_string()),
        ];

        let auth = build_authorization_header(
            &credentials,
            "POST",
            "/model/anthropic.claude-3-sonnet/converse",
            "",
            &headers,
            b"{}",
            &timestamp,
        );

        // Verify structure
        assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/"));
        assert!(auth.contains("SignedHeaders=content-type;host;x-amz-date"));
        assert!(auth.contains("Signature="));
        assert!(auth.contains("/us-east-1/bedrock/aws4_request"));
    }

    #[test]
    fn build_authorization_header_includes_security_token_in_signed_headers() {
        let credentials = AwsCredentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            session_token: Some("session-token-value".to_string()),
            region: "us-east-1".to_string(),
            expires_at: None,
        };

        let timestamp = chrono::DateTime::parse_from_rfc3339("2024-01-15T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        let headers = vec![
            ("content-type".to_string(), "application/json".to_string()),
            (
                "host".to_string(),
                "bedrock-runtime.us-east-1.amazonaws.com".to_string(),
            ),
            ("x-amz-date".to_string(), "20240115T120000Z".to_string()),
            (
                "x-amz-security-token".to_string(),
                "session-token-value".to_string(),
            ),
        ];

        let auth = build_authorization_header(
            &credentials,
            "POST",
            "/model/test-model/converse",
            "",
            &headers,
            b"{}",
            &timestamp,
        );

        assert!(auth.contains("x-amz-security-token"));
    }

    // ── Credential tests ────────────────────────────────────────

    #[test]
    fn credentials_host_formats_correctly() {
        let creds = AwsCredentials {
            access_key_id: "AKID".to_string(),
            secret_access_key: "secret".to_string(),
            session_token: None,
            region: "us-west-2".to_string(),
            expires_at: None,
        };
        assert_eq!(creds.host(), "bedrock-runtime.us-west-2.amazonaws.com");
    }

    // ── ModelProvider construction tests ─────────────────────────────

    #[test]
    fn creates_without_credentials() {
        // ModelProvider should construct even without env vars.
        let _provider = BedrockModelProvider::new();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn chat_fails_without_credentials() {
        let _env_lock = env_lock();
        let _ak = EnvGuard::set("AWS_ACCESS_KEY_ID", None);
        let _sk = EnvGuard::set("AWS_SECRET_ACCESS_KEY", None);
        let _bearer = EnvGuard::set("BEDROCK_API_KEY", None);
        let _config = EnvGuard::set("AWS_CONFIG_FILE", Some("/dev/null"));
        let model_provider = BedrockModelProvider {
            auth: None,
            max_tokens: zeroclaw_api::model_provider::BASELINE_MAX_TOKENS,
            cred_cache: Mutex::new(None),
        };
        let result = model_provider
            .chat_with_system(None, "hello", "anthropic.claude-sonnet-4-6", Some(0.7))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("credentials not set")
                || err.contains("169.254.169.254")
                || err.to_lowercase().contains("credential")
                || err.to_lowercase().contains("builder error"),
            "Expected missing-credentials style error, got: {err}"
        );
    }

    // ── Bearer token tests ──────────────────────────────────────

    #[test]
    fn creates_with_bearer_token() {
        let model_provider = BedrockModelProvider::with_bearer_token("test-api-key");
        assert!(model_provider.auth.is_some());
        assert!(
            matches!(model_provider.auth, Some(BedrockAuth::BearerToken(ref t)) if t == "test-api-key")
        );
    }

    #[test]
    fn bearer_token_from_env() {
        let _env_lock = env_lock();
        let _guard = EnvGuard::set("BEDROCK_API_KEY", Some("env-bearer-token"));
        // Clear SigV4 vars to ensure Bearer is chosen.
        let _ak_guard = EnvGuard::set("AWS_ACCESS_KEY_ID", None);
        let _sk_guard = EnvGuard::set("AWS_SECRET_ACCESS_KEY", None);

        let model_provider = BedrockModelProvider::new();
        assert!(matches!(
            model_provider.auth,
            Some(BedrockAuth::BearerToken(ref t)) if t == "env-bearer-token"
        ));
    }

    #[test]
    fn bearer_token_precedence() {
        let _env_lock = env_lock();
        let _bearer_guard = EnvGuard::set("BEDROCK_API_KEY", Some("bearer-key"));
        let _ak_guard = EnvGuard::set("AWS_ACCESS_KEY_ID", Some("AKIAEXAMPLE"));
        let _sk_guard = EnvGuard::set("AWS_SECRET_ACCESS_KEY", Some("secret"));

        let model_provider = BedrockModelProvider::new();
        // Bearer token should take priority over SigV4 credentials.
        assert!(matches!(
            model_provider.auth,
            Some(BedrockAuth::BearerToken(ref t)) if t == "bearer-key"
        ));
    }

    // ── Endpoint URL tests ──────────────────────────────────────

    #[test]
    fn endpoint_url_formats_correctly() {
        let url = BedrockModelProvider::endpoint_url("us-east-1", "anthropic.claude-sonnet-4-6");
        assert_eq!(
            url,
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude-sonnet-4-6/converse"
        );
    }

    #[test]
    fn endpoint_url_keeps_raw_colon() {
        // Endpoint URL uses raw colon so reqwest sends `:` on the wire.
        let url = BedrockModelProvider::endpoint_url(
            "us-west-2",
            "anthropic.claude-3-5-haiku-20241022-v1:0",
        );
        assert!(url.contains("/model/anthropic.claude-3-5-haiku-20241022-v1:0/converse"));
    }

    #[test]
    fn canonical_uri_encodes_colon() {
        // Canonical URI must encode `:` as `%3A` for SigV4 signing.
        let uri = BedrockModelProvider::canonical_uri("anthropic.claude-3-5-haiku-20241022-v1:0");
        assert_eq!(
            uri,
            "/model/anthropic.claude-3-5-haiku-20241022-v1%3A0/converse"
        );
    }

    #[test]
    fn canonical_uri_no_colon_unchanged() {
        let uri = BedrockModelProvider::canonical_uri("anthropic.claude-sonnet-4-6");
        assert_eq!(uri, "/model/anthropic.claude-sonnet-4-6/converse");
    }

    // ── Message conversion tests ────────────────────────────────

    #[test]
    fn convert_messages_system_extracted() {
        let messages = vec![
            ChatMessage::system("You are helpful"),
            ChatMessage::user("Hello"),
        ];
        let (system, msgs) = BedrockModelProvider::convert_messages(&messages);
        assert!(system.is_some());
        let system_blocks = system.unwrap();
        assert_eq!(system_blocks.len(), 1);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
    }

    #[test]
    fn convert_messages_user_and_assistant() {
        let messages = vec![
            ChatMessage::user("Hello"),
            ChatMessage::assistant("Hi there"),
        ];
        let (system, msgs) = BedrockModelProvider::convert_messages(&messages);
        assert!(system.is_none());
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].role, "assistant");
    }

    #[test]
    fn convert_messages_tool_role_to_tool_result() {
        let tool_json = r#"{"tool_call_id": "call_123", "content": "Result data"}"#;
        let messages = vec![ChatMessage::tool(tool_json)];
        let (_, msgs) = BedrockModelProvider::convert_messages(&messages);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
        assert!(matches!(msgs[0].content[0], ContentBlock::ToolResult(_)));
    }

    #[test]
    fn convert_messages_assistant_tool_calls_parsed() {
        let tool_call_json = r#"{"content": "Let me check", "tool_calls": [{"id": "call_1", "name": "shell", "arguments": "{\"command\":\"ls\"}"}]}"#;
        let messages = vec![ChatMessage::assistant(tool_call_json)];
        let (_, msgs) = BedrockModelProvider::convert_messages(&messages);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "assistant");
        assert_eq!(msgs[0].content.len(), 2);
        assert!(matches!(msgs[0].content[0], ContentBlock::Text(_)));
        assert!(matches!(msgs[0].content[1], ContentBlock::ToolUse(_)));
    }

    #[test]
    fn convert_messages_plain_assistant_text() {
        let messages = vec![ChatMessage::assistant("Just text")];
        let (_, msgs) = BedrockModelProvider::convert_messages(&messages);
        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0].content[0], ContentBlock::Text(_)));
    }

    // ── Cache tests ─────────────────────────────────────────────

    #[test]
    fn should_cache_system_small_prompt() {
        assert!(!BedrockModelProvider::should_cache_system("Short prompt"));
    }

    #[test]
    fn should_cache_system_large_prompt() {
        let large = "a".repeat(3073);
        assert!(BedrockModelProvider::should_cache_system(&large));
    }

    #[test]
    fn should_cache_system_boundary() {
        assert!(!BedrockModelProvider::should_cache_system(
            &"a".repeat(3072)
        ));
        assert!(BedrockModelProvider::should_cache_system(&"a".repeat(3073)));
    }

    #[test]
    fn should_cache_conversation_short() {
        let messages = vec![
            ChatMessage::system("System"),
            ChatMessage::user("Hello"),
            ChatMessage::assistant("Hi"),
        ];
        assert!(!BedrockModelProvider::should_cache_conversation(&messages));
    }

    #[test]
    fn should_cache_conversation_long() {
        let mut messages = vec![ChatMessage::system("System")];
        for i in 0..5 {
            messages.push(ChatMessage {
                role: if i % 2 == 0 { "user" } else { "assistant" }.to_string(),
                content: format!("Message {i}"),
            });
        }
        assert!(BedrockModelProvider::should_cache_conversation(&messages));
    }

    // ── Tool conversion tests ───────────────────────────────────

    #[test]
    fn convert_tools_to_converse_formats_correctly() {
        let tools = vec![ToolSpec {
            name: "shell".to_string(),
            description: "Run commands".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
        }];
        let config = BedrockModelProvider::convert_tools_to_converse(Some(&tools));
        assert!(config.is_some());
        let config = config.unwrap();
        assert_eq!(config.tools.len(), 1);
        assert_eq!(config.tools[0].tool_spec.name, "shell");
    }

    #[test]
    fn convert_tools_to_converse_empty_returns_none() {
        assert!(BedrockModelProvider::convert_tools_to_converse(Some(&[])).is_none());
        assert!(BedrockModelProvider::convert_tools_to_converse(None).is_none());
    }

    // ── Serde tests ─────────────────────────────────────────────

    #[test]
    fn converse_request_serializes_without_system() {
        let req = ConverseRequest {
            system: None,
            messages: vec![ConverseMessage {
                role: "user".to_string(),
                content: vec![ContentBlock::Text(TextBlock {
                    text: "Hello".to_string(),
                })],
            }],
            inference_config: Some(InferenceConfig {
                max_tokens: 4096,
                temperature: Some(0.7),
            }),
            tool_config: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("system"));
        assert!(json.contains("Hello"));
        assert!(json.contains("maxTokens"));
    }

    // ── Opus 4.7 temperature-omission tests (issue #6095) ────────

    #[test]
    fn bedrock_model_omits_temperature_matches_opus_4_7() {
        assert!(bedrock_model_omits_temperature(
            "us.anthropic.claude-opus-4-7"
        ));
        assert!(bedrock_model_omits_temperature(
            "anthropic.claude-opus-4-7-v1:0"
        ));
    }

    #[test]
    fn bedrock_model_omits_temperature_skips_other_models() {
        assert!(!bedrock_model_omits_temperature(
            "us.anthropic.claude-opus-4-6-v1"
        ));
        assert!(!bedrock_model_omits_temperature(
            "us.anthropic.claude-sonnet-4-6-v1"
        ));
        assert!(!bedrock_model_omits_temperature(
            "us.anthropic.claude-haiku-4-5-v1"
        ));
    }

    #[test]
    fn inference_config_serializes_without_temperature_when_none() {
        let cfg = InferenceConfig {
            max_tokens: 4096,
            temperature: None,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("maxTokens"));
        assert!(
            !json.contains("temperature"),
            "expected temperature to be omitted, got: {json}"
        );
    }

    #[test]
    fn inference_config_serializes_with_temperature_when_some() {
        let cfg = InferenceConfig {
            max_tokens: 4096,
            temperature: Some(0.7),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("maxTokens"));
        assert!(
            json.contains("temperature"),
            "expected temperature to be present, got: {json}"
        );
    }

    #[test]
    fn converse_response_deserializes_text() {
        let json = r#"{
            "output": {
                "message": {
                    "role": "assistant",
                    "content": [{"text": "Hello from Bedrock"}]
                }
            },
            "stopReason": "end_turn"
        }"#;
        let resp: ConverseResponse = serde_json::from_str(json).unwrap();
        let parsed = BedrockModelProvider::parse_converse_response(resp);
        assert_eq!(parsed.text.as_deref(), Some("Hello from Bedrock"));
        assert!(parsed.tool_calls.is_empty());
    }

    #[test]
    fn converse_response_deserializes_tool_use() {
        let json = r#"{
            "output": {
                "message": {
                    "role": "assistant",
                    "content": [
                        {"toolUse": {"toolUseId": "call_1", "name": "shell", "input": {"command": "ls"}}}
                    ]
                }
            },
            "stopReason": "tool_use"
        }"#;
        let resp: ConverseResponse = serde_json::from_str(json).unwrap();
        let parsed = BedrockModelProvider::parse_converse_response(resp);
        assert!(parsed.text.is_none());
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "shell");
        assert_eq!(parsed.tool_calls[0].id, "call_1");
    }

    #[test]
    fn converse_response_empty_output() {
        let json = r#"{"output": null, "stopReason": null}"#;
        let resp: ConverseResponse = serde_json::from_str(json).unwrap();
        let parsed = BedrockModelProvider::parse_converse_response(resp);
        assert!(parsed.text.is_none());
        assert!(parsed.tool_calls.is_empty());
    }

    #[test]
    fn content_block_text_serializes_as_flat_string() {
        let block = ContentBlock::Text(TextBlock {
            text: "Hello".to_string(),
        });
        let json = serde_json::to_string(&block).unwrap();
        // Must be {"text":"Hello"}, NOT {"text":{"text":"Hello"}}
        assert_eq!(json, r#"{"text":"Hello"}"#);
    }

    #[test]
    fn content_block_tool_use_serializes_with_nested_object() {
        let block = ContentBlock::ToolUse(ToolUseWrapper {
            tool_use: ToolUseBlock {
                tool_use_id: "call_1".to_string(),
                name: "shell".to_string(),
                input: serde_json::json!({"command": "ls"}),
            },
        });
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains(r#""toolUse""#));
        assert!(json.contains(r#""toolUseId":"call_1""#));
    }

    #[test]
    fn content_block_cache_point_serializes() {
        let block = ContentBlock::CachePointBlock(CachePointWrapper {
            cache_point: CachePoint::default_cache(),
        });
        let json = serde_json::to_string(&block).unwrap();
        assert_eq!(json, r#"{"cachePoint":{"type":"default"}}"#);
    }

    #[test]
    fn content_block_text_round_trips() {
        let original = ContentBlock::Text(TextBlock {
            text: "Hello".to_string(),
        });
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: ContentBlock = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, ContentBlock::Text(tb) if tb.text == "Hello"));
    }

    #[test]
    fn cache_point_serializes() {
        let cp = CachePoint::default_cache();
        let json = serde_json::to_string(&cp).unwrap();
        assert_eq!(json, r#"{"type":"default"}"#);
    }

    #[tokio::test]
    async fn warmup_without_credentials_is_noop() {
        let model_provider = BedrockModelProvider {
            auth: None,
            max_tokens: zeroclaw_api::model_provider::BASELINE_MAX_TOKENS,
            cred_cache: Mutex::new(None),
        };
        let result = model_provider.warmup().await;
        assert!(result.is_ok());
    }

    #[test]
    fn capabilities_reports_native_tool_calling() {
        let model_provider = BedrockModelProvider {
            auth: None,
            max_tokens: zeroclaw_api::model_provider::BASELINE_MAX_TOKENS,
            cred_cache: Mutex::new(None),
        };
        let caps = model_provider.capabilities();
        assert!(caps.native_tool_calling);
    }

    #[test]
    fn converse_response_parses_usage() {
        let json = r#"{
            "output": {"message": {"role": "assistant", "content": [{"text": {"text": "Hello"}}]}},
            "usage": {"inputTokens": 500, "outputTokens": 100}
        }"#;
        let resp: ConverseResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage.unwrap();
        assert_eq!(usage.input_tokens, Some(500));
        assert_eq!(usage.output_tokens, Some(100));
    }

    #[test]
    fn converse_response_parses_without_usage() {
        let json = r#"{"output": {"message": {"role": "assistant", "content": []}}}"#;
        let resp: ConverseResponse = serde_json::from_str(json).unwrap();
        assert!(resp.usage.is_none());
    }

    // ── Tool result fallback & merge tests ───────────────────────

    #[test]
    fn fallback_tool_result_emits_tool_result_block_not_text() {
        // When tool message content is not valid JSON, we should still get
        // a toolResult block (not a plain text user message).
        let messages = vec![
            ChatMessage::user("do something"),
            ChatMessage::assistant(
                r#"{"content":"","tool_calls":[{"id":"tool_1","name":"shell","arguments":"{}"}]}"#,
            ),
            ChatMessage {
                role: "tool".to_string(),
                content: "not valid json".to_string(),
            },
        ];
        let (_, msgs) = BedrockModelProvider::convert_messages(&messages);
        let tool_msg = &msgs[2];
        assert_eq!(tool_msg.role, "user");
        assert!(
            matches!(&tool_msg.content[0], ContentBlock::ToolResult(_)),
            "Expected ToolResult block, got {:?}",
            tool_msg.content[0]
        );
    }

    #[test]
    fn fallback_recovers_tool_use_id_from_assistant() {
        let messages = vec![
            ChatMessage::user("run it"),
            ChatMessage::assistant(
                r#"{"content":"","tool_calls":[{"id":"tool_abc","name":"shell","arguments":"{}"}]}"#,
            ),
            ChatMessage {
                role: "tool".to_string(),
                content: "raw output with no json".to_string(),
            },
        ];
        let (_, msgs) = BedrockModelProvider::convert_messages(&messages);
        if let ContentBlock::ToolResult(ref wrapper) = msgs[2].content[0] {
            assert_eq!(wrapper.tool_result.tool_use_id, "tool_abc");
            assert_eq!(wrapper.tool_result.status, "error");
        } else {
            panic!("Expected ToolResult block");
        }
    }

    #[test]
    fn consecutive_tool_results_merged_into_single_message() {
        let messages = vec![
            ChatMessage::user("do two things"),
            ChatMessage::assistant(
                r#"{"content":"","tool_calls":[{"id":"t1","name":"a","arguments":"{}"},{"id":"t2","name":"b","arguments":"{}"}]}"#,
            ),
            ChatMessage::tool(r#"{"tool_call_id":"t1","content":"result 1"}"#),
            ChatMessage::tool(r#"{"tool_call_id":"t2","content":"result 2"}"#),
        ];
        let (_, msgs) = BedrockModelProvider::convert_messages(&messages);
        // Should be: user, assistant, user (merged tool results)
        assert_eq!(msgs.len(), 3, "Expected 3 messages, got {}", msgs.len());
        assert_eq!(msgs[2].role, "user");
        assert_eq!(
            msgs[2].content.len(),
            2,
            "Expected 2 tool results in one message"
        );
        assert!(matches!(&msgs[2].content[0], ContentBlock::ToolResult(_)));
        assert!(matches!(&msgs[2].content[1], ContentBlock::ToolResult(_)));
    }

    #[test]
    fn extract_tool_call_id_tries_multiple_field_names() {
        assert_eq!(
            BedrockModelProvider::extract_tool_call_id(r#"{"tool_call_id":"a"}"#),
            Some("a".to_string())
        );
        assert_eq!(
            BedrockModelProvider::extract_tool_call_id(r#"{"tool_use_id":"b"}"#),
            Some("b".to_string())
        );
        assert_eq!(
            BedrockModelProvider::extract_tool_call_id(r#"{"toolUseId":"c"}"#),
            Some("c".to_string())
        );
        assert_eq!(
            BedrockModelProvider::extract_tool_call_id("not json at all"),
            None
        );
    }

    #[test]
    fn parse_tool_result_accepts_alternate_id_fields() {
        let msg = BedrockModelProvider::parse_tool_result_message(
            r#"{"tool_use_id":"x","content":"ok"}"#,
        );
        assert!(msg.is_some());
        if let ContentBlock::ToolResult(ref wrapper) = msg.unwrap().content[0] {
            assert_eq!(wrapper.tool_result.tool_use_id, "x");
        } else {
            panic!("Expected ToolResult");
        }
    }

    #[test]
    fn sanitize_removes_empty_text_blocks() {
        let mut messages = vec![ConverseMessage {
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text(TextBlock {
                text: String::new(),
            })],
        }];
        BedrockModelProvider::sanitize_empty_content_blocks(&mut messages);
        assert_eq!(messages.len(), 1);
        if let ContentBlock::Text(ref tb) = messages[0].content[0] {
            assert_eq!(tb.text, "(empty)");
        } else {
            panic!("Expected Text block with placeholder");
        }
    }

    #[test]
    fn sanitize_preserves_non_empty_text_blocks() {
        let mut messages = vec![ConverseMessage {
            role: "user".to_string(),
            content: vec![ContentBlock::Text(TextBlock {
                text: "Hello".to_string(),
            })],
        }];
        BedrockModelProvider::sanitize_empty_content_blocks(&mut messages);
        if let ContentBlock::Text(ref tb) = messages[0].content[0] {
            assert_eq!(tb.text, "Hello");
        } else {
            panic!("Expected preserved Text block");
        }
    }

    #[test]
    fn convert_messages_empty_assistant_gets_placeholder() {
        let messages = vec![
            ChatMessage::user("Hello"),
            ChatMessage {
                role: "assistant".to_string(),
                content: String::new(),
            },
            ChatMessage::user("Continue"),
        ];
        let (_, converse) = BedrockModelProvider::convert_messages(&messages);
        let assistant_msg = &converse[1];
        assert_eq!(assistant_msg.role, "assistant");
        if let ContentBlock::Text(ref tb) = assistant_msg.content[0] {
            assert!(!tb.text.is_empty(), "Assistant text should not be empty");
        } else {
            panic!("Expected Text block for assistant message");
        }
    }

    // ── credential_process tests ────────────────────────────────

    #[test]
    fn parse_aws_config_default_profile() {
        let config = "\
[default]
region=us-west-2
credential_process=ada credentials print --account=123 --provider=conduit --role=MyRole
";
        let result = AwsCredentials::parse_aws_config(config, "default");
        assert!(result.is_some());
        let (cmd, region) = result.unwrap();
        assert_eq!(
            cmd,
            "ada credentials print --account=123 --provider=conduit --role=MyRole"
        );
        assert_eq!(region.as_deref(), Some("us-west-2"));
    }

    #[test]
    fn parse_aws_config_named_profile() {
        let config = "\
[default]
region=us-east-1

[profile myprofile]
region=eu-west-1
credential_process=aws sso get-role-credentials --profile myprofile
";
        let result = AwsCredentials::parse_aws_config(config, "myprofile");
        assert!(result.is_some());
        let (cmd, region) = result.unwrap();
        assert!(cmd.contains("myprofile"));
        assert_eq!(region.as_deref(), Some("eu-west-1"));
    }

    #[test]
    fn parse_aws_config_missing_credential_process() {
        let config = "\
[default]
region=us-west-2
";
        let result = AwsCredentials::parse_aws_config(config, "default");
        assert!(result.is_none());
    }

    #[test]
    fn parse_aws_config_ignores_comments() {
        let config = "\
[default]
# credential_process=should-be-ignored
; credential_process=also-ignored
credential_process=real-command
";
        let result = AwsCredentials::parse_aws_config(config, "default");
        assert!(result.is_some());
        assert_eq!(result.unwrap().0, "real-command");
    }

    #[test]
    fn parse_aws_config_nonexistent_profile() {
        let config = "\
[default]
credential_process=some-command
";
        let result = AwsCredentials::parse_aws_config(config, "nonexistent");
        assert!(result.is_none());
    }

    #[test]
    fn from_credential_process_parses_json_output() {
        // Verify config parsing + JSON shape by using `echo` as the command.
        let config = "\
[default]
credential_process=echo '{\"Version\":1,\"AccessKeyId\":\"AKIA\",\"SecretAccessKey\":\"secret\",\"SessionToken\":\"tok\"}'
region=ap-southeast-1
";
        let (cmd, region) = AwsCredentials::parse_aws_config(config, "default").unwrap();
        assert!(cmd.starts_with("echo"));
        assert_eq!(region.as_deref(), Some("ap-southeast-1"));

        let output = std::process::Command::new("sh")
            .args(["-c", &cmd])
            .output()
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(json["AccessKeyId"].as_str(), Some("AKIA"));
        assert_eq!(json["SecretAccessKey"].as_str(), Some("secret"));
        assert_eq!(json["SessionToken"].as_str(), Some("tok"));
    }

    #[test]
    fn env_vars_take_precedence_over_credential_process() {
        let _env_lock = env_lock();
        let _ak = EnvGuard::set("AWS_ACCESS_KEY_ID", Some("FROM_ENV"));
        let _sk = EnvGuard::set("AWS_SECRET_ACCESS_KEY", Some("secret_from_env"));

        let creds = AwsCredentials::from_env();
        assert!(creds.is_ok());
        assert_eq!(creds.unwrap().access_key_id, "FROM_ENV");
    }

    // ── credential cache tests ──────────────────────────────────

    fn make_creds(expires_at: Option<chrono::DateTime<chrono::Utc>>) -> AwsCredentials {
        AwsCredentials {
            access_key_id: "AKIA".to_string(),
            secret_access_key: "secret".to_string(),
            session_token: Some("tok".to_string()),
            region: "us-west-2".to_string(),
            expires_at,
        }
    }

    #[test]
    fn is_expired_returns_false_when_no_expiry() {
        let creds = make_creds(None);
        assert!(!creds.is_expired());
    }

    #[test]
    fn is_expired_returns_false_when_future() {
        let future = chrono::Utc::now() + chrono::Duration::hours(1);
        let creds = make_creds(Some(future));
        assert!(!creds.is_expired());
    }

    #[test]
    fn is_expired_returns_true_when_past() {
        let past = chrono::Utc::now() - chrono::Duration::hours(1);
        let creds = make_creds(Some(past));
        assert!(creds.is_expired());
    }

    #[test]
    fn is_expired_returns_true_within_skew_window() {
        // 30 seconds from now is within the 60s skew — should be treated as expired.
        let soon = chrono::Utc::now() + chrono::Duration::seconds(30);
        let creds = make_creds(Some(soon));
        assert!(creds.is_expired());
    }

    #[test]
    fn cached_credentials_returns_none_when_empty() {
        let model_provider = BedrockModelProvider {
            auth: None,
            max_tokens: zeroclaw_api::model_provider::BASELINE_MAX_TOKENS,
            cred_cache: Mutex::new(None),
        };
        assert!(model_provider.cached_credentials().is_none());
    }

    #[test]
    fn cached_credentials_returns_some_when_valid() {
        let future = chrono::Utc::now() + chrono::Duration::hours(1);
        let model_provider = BedrockModelProvider {
            auth: None,
            max_tokens: zeroclaw_api::model_provider::BASELINE_MAX_TOKENS,
            cred_cache: Mutex::new(Some(make_creds(Some(future)))),
        };
        let cached = model_provider.cached_credentials();
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().access_key_id, "AKIA");
    }

    #[test]
    fn cached_credentials_returns_none_when_expired() {
        let past = chrono::Utc::now() - chrono::Duration::hours(1);
        let model_provider = BedrockModelProvider {
            auth: None,
            max_tokens: zeroclaw_api::model_provider::BASELINE_MAX_TOKENS,
            cred_cache: Mutex::new(Some(make_creds(Some(past)))),
        };
        assert!(model_provider.cached_credentials().is_none());
    }

    #[test]
    fn cache_credentials_stores_and_retrieves() {
        let future = chrono::Utc::now() + chrono::Duration::hours(1);
        let model_provider = BedrockModelProvider {
            auth: None,
            max_tokens: zeroclaw_api::model_provider::BASELINE_MAX_TOKENS,
            cred_cache: Mutex::new(None),
        };
        assert!(model_provider.cached_credentials().is_none());
        model_provider.cache_credentials(&make_creds(Some(future)));
        assert!(model_provider.cached_credentials().is_some());
    }
}
