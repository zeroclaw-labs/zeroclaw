//! AWS Bedrock provider using the Converse API.
//!
//! Authentication: AWS AKSK (Access Key ID + Secret Access Key)
//! via environment variables. SigV4 signing is implemented manually
//! using hmac/sha2 crates — no AWS SDK dependency.

use crate::providers::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    NormalizedStopReason, Provider, ProviderCapabilities, StreamChunk, StreamError, StreamOptions,
    StreamResult, TokenUsage, ToolCall as ProviderToolCall, ToolsPayload,
};
use crate::tools::ToolSpec;
use async_trait::async_trait;
use futures_util::{stream, StreamExt};
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

/// Hostname prefix for the Bedrock Runtime endpoint.
const ENDPOINT_PREFIX: &str = "bedrock-runtime";
/// SigV4 signing service name (AWS uses "bedrock", not "bedrock-runtime").
const SIGNING_SERVICE: &str = "bedrock";
const DEFAULT_REGION: &str = "us-east-1";
const DEFAULT_MAX_TOKENS: u32 = 4096;

// ── AWS Credentials ─────────────────────────────────────────────

/// Resolved AWS credentials for SigV4 signing.
#[derive(Clone)]
struct AwsCredentials {
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
    region: String,
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
        })
    }

    /// Fetch credentials from ECS container credential endpoint.
    /// Available when running on ECS/Fargate with a task IAM role.
    async fn from_ecs() -> anyhow::Result<Self> {
        // Try relative URI first (standard ECS), then full URI (ECS Anywhere / custom)
        let uri = std::env::var("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")
            .ok()
            .map(|rel| format!("http://169.254.170.2{rel}"))
            .or_else(|| std::env::var("AWS_CONTAINER_CREDENTIALS_FULL_URI").ok());

        let uri = uri.ok_or_else(|| {
            anyhow::anyhow!(
                "Neither AWS_CONTAINER_CREDENTIALS_RELATIVE_URI nor \
                 AWS_CONTAINER_CREDENTIALS_FULL_URI is set"
            )
        })?;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()?;

        let mut req = client.get(&uri);
        // ECS Anywhere / full URI may require an authorization token
        if let Ok(token) = std::env::var("AWS_CONTAINER_AUTHORIZATION_TOKEN") {
            req = req.header("Authorization", token);
        }

        let creds_json: serde_json::Value = req.send().await?.json().await?;

        let access_key_id = creds_json["AccessKeyId"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing AccessKeyId in ECS credential response"))?
            .to_string();
        let secret_access_key = creds_json["SecretAccessKey"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing SecretAccessKey in ECS credential response"))?
            .to_string();
        let session_token = creds_json["Token"].as_str().map(|s| s.to_string());

        let region = env_optional("AWS_REGION")
            .or_else(|| env_optional("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|| DEFAULT_REGION.to_string());

        tracing::info!("Loaded AWS credentials from ECS container credential endpoint");

        Ok(Self {
            access_key_id,
            secret_access_key,
            session_token,
            region,
        })
    }

    /// Resolve credentials: env vars → ECS endpoint → EC2 IMDS.
    async fn resolve() -> anyhow::Result<Self> {
        if let Ok(creds) = Self::from_env() {
            return Ok(creds);
        }
        if let Ok(creds) = Self::from_ecs().await {
            return Ok(creds);
        }
        Self::from_imds().await
    }

    fn host(&self) -> String {
        format!("{ENDPOINT_PREFIX}.{}.amazonaws.com", self.region)
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

/// How long credentials are considered fresh before re-fetching.
/// ECS STS tokens typically expire after 6-12 hours; we refresh well
/// before that to avoid any requests hitting expired tokens.
const CREDENTIAL_TTL_SECS: u64 = 50 * 60; // 50 minutes

/// Thread-safe credential cache that auto-refreshes from the ECS
/// container credential endpoint (or env vars / IMDS) when the
/// cached credentials are older than [`CREDENTIAL_TTL_SECS`].
struct CachedCredentials {
    inner: Arc<RwLock<Option<(AwsCredentials, Instant)>>>,
}

impl CachedCredentials {
    /// Create a new cache, optionally pre-populated with initial credentials.
    fn new(initial: Option<AwsCredentials>) -> Self {
        let entry = initial.map(|c| (c, Instant::now()));
        Self {
            inner: Arc::new(RwLock::new(entry)),
        }
    }

    /// Get current credentials, refreshing if stale or missing.
    async fn get(&self) -> anyhow::Result<AwsCredentials> {
        // Fast path: read lock, check freshness
        {
            let guard = self.inner.read().await;
            if let Some((ref creds, fetched_at)) = *guard {
                if fetched_at.elapsed().as_secs() < CREDENTIAL_TTL_SECS {
                    return Ok(creds.clone());
                }
            }
        }

        // Slow path: write lock, re-fetch
        let mut guard = self.inner.write().await;
        // Double-check after acquiring write lock (another task may have refreshed)
        if let Some((ref creds, fetched_at)) = *guard {
            if fetched_at.elapsed().as_secs() < CREDENTIAL_TTL_SECS {
                return Ok(creds.clone());
            }
        }

        tracing::info!("Refreshing AWS credentials (TTL expired or first fetch)");
        let fresh = AwsCredentials::resolve().await?;
        let cloned = fresh.clone();
        *guard = Some((fresh, Instant::now()));
        Ok(cloned)
    }
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
    temperature: f64,
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
    Other(serde_json::Value),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResponseToolUseWrapper {
    tool_use: ToolUseBlock,
}

// ── BedrockProvider ─────────────────────────────────────────────

pub struct BedrockProvider {
    credentials: CachedCredentials,
}

impl BedrockProvider {
    pub fn new() -> Self {
        Self {
            credentials: CachedCredentials::new(AwsCredentials::from_env().ok()),
        }
    }

    pub async fn new_async() -> Self {
        let initial = AwsCredentials::resolve().await.ok();
        Self {
            credentials: CachedCredentials::new(initial),
        }
    }

    fn http_client(&self) -> Client {
        crate::config::build_runtime_proxy_client_with_timeouts("provider.bedrock", 120, 10)
    }

    /// Percent-encode the model ID for URL path: only encode `:` to `%3A`.
    /// Colons in model IDs (e.g. `v1:0`) must be encoded because `reqwest::Url`
    /// may misparse them. Dots, hyphens, and alphanumerics are safe.
    fn encode_model_path(model_id: &str) -> String {
        model_id.replace(':', "%3A")
    }

    /// Build the actual request URL. Uses raw model ID (reqwest sends colons as-is).
    fn endpoint_url(region: &str, model_id: &str) -> String {
        format!("https://{ENDPOINT_PREFIX}.{region}.amazonaws.com/model/{model_id}/converse")
    }

    /// Build the streaming request URL (converse-stream endpoint).
    fn stream_endpoint_url(region: &str, model_id: &str) -> String {
        format!("https://{ENDPOINT_PREFIX}.{region}.amazonaws.com/model/{model_id}/converse-stream")
    }

    /// Build the canonical URI for SigV4 signing. Must URI-encode the path
    /// per SigV4 spec: colons become `%3A`. AWS verifies the signature against
    /// the encoded form even though the wire request uses raw colons.
    fn canonical_uri(model_id: &str) -> String {
        let encoded = Self::encode_model_path(model_id);
        format!("/model/{encoded}/converse")
    }

    /// Canonical URI for the streaming endpoint.
    fn stream_canonical_uri(model_id: &str) -> String {
        let encoded = Self::encode_model_path(model_id);
        format!("/model/{encoded}/converse-stream")
    }

    /// Get credentials, auto-refreshing from the ECS endpoint / env vars /
    /// IMDS when they are older than [`CREDENTIAL_TTL_SECS`].
    async fn get_credentials(&self) -> anyhow::Result<AwsCredentials> {
        self.credentials.get().await
    }

    // ── Cache heuristics (same thresholds as AnthropicProvider) ──

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
                        converse_messages.push(ConverseMessage {
                            role: "assistant".to_string(),
                            content: vec![ContentBlock::Text(TextBlock {
                                text: msg.content.clone(),
                            })],
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
                    if let Some(last) = converse_messages.last_mut() {
                        if last.role == "user"
                            && last
                                .content
                                .iter()
                                .all(|b| matches!(b, ContentBlock::ToolResult(_)))
                        {
                            last.content.extend(tool_result_msg.content);
                            continue;
                        }
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
                if let Some(rest) = src.strip_prefix("data:") {
                    if let Some(semi) = rest.find(';') {
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
            blocks.push(ContentBlock::Text(TextBlock {
                text: content.to_string(),
            }));
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
        let raw_stop_reason = response.stop_reason.clone();
        let stop_reason = raw_stop_reason
            .as_deref()
            .map(NormalizedStopReason::from_bedrock_stop_reason);

        let usage = response.usage.map(|u| TokenUsage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
        });

        if let Some(output) = response.output {
            if let Some(message) = output.message {
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
                                });
                            }
                        }
                        ResponseContentBlock::Other(_) => {}
                    }
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
            quota_metadata: None,
            stop_reason,
            raw_stop_reason,
        }
    }

    // ── HTTP request ────────────────────────────────────────────

    async fn send_converse_request(
        &self,
        credentials: &AwsCredentials,
        model: &str,
        request_body: &ConverseRequest,
    ) -> anyhow::Result<ConverseResponse> {
        let payload = serde_json::to_vec(request_body)?;

        // Debug: log image blocks in payload (truncated)
        if let Ok(debug_val) = serde_json::from_slice::<serde_json::Value>(&payload) {
            if let Some(msgs) = debug_val.get("messages").and_then(|m| m.as_array()) {
                for msg in msgs {
                    if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
                        for block in content {
                            if block.get("image").is_some() {
                                let mut b = block.clone();
                                if let Some(img) = b.get_mut("image") {
                                    if let Some(src) = img.get_mut("source") {
                                        if let Some(bytes) = src.get_mut("bytes") {
                                            if let Some(s) = bytes.as_str() {
                                                *bytes = serde_json::json!(format!(
                                                    "<base64 {} chars>",
                                                    s.len()
                                                ));
                                            }
                                        }
                                    }
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
        }
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
        if let Some(ref token) = credentials.session_token {
            headers_to_sign.push(("x-amz-security-token".to_string(), token.clone()));
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

        if let Some(ref token) = credentials.session_token {
            request = request.header("x-amz-security-token", token);
        }

        let response: reqwest::Response = request.body(payload).send().await?;

        if !response.status().is_success() {
            return Err(super::api_error("Bedrock", response).await);
        }

        let converse_response: ConverseResponse = response.json().await?;
        Ok(converse_response)
    }

    /// Send a signed request to the ConverseStream endpoint and return the raw
    /// response for event-stream parsing.
    async fn send_converse_stream_request(
        &self,
        credentials: &AwsCredentials,
        model: &str,
        request_body: &ConverseRequest,
    ) -> anyhow::Result<reqwest::Response> {
        let payload = serde_json::to_vec(request_body)?;
        let url = Self::stream_endpoint_url(&credentials.region, model);
        let canonical_uri = Self::stream_canonical_uri(model);
        let now = chrono::Utc::now();
        let host = credentials.host();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();

        let mut headers_to_sign = vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("host".to_string(), host),
            ("x-amz-date".to_string(), amz_date.clone()),
        ];
        if let Some(ref token) = credentials.session_token {
            headers_to_sign.push(("x-amz-security-token".to_string(), token.clone()));
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

        if let Some(ref token) = credentials.session_token {
            request = request.header("x-amz-security-token", token);
        }

        let response = request.body(payload).send().await?;

        if !response.status().is_success() {
            return Err(super::api_error("Bedrock", response).await);
        }

        Ok(response)
    }
}

// ── AWS Event-Stream Binary Parser ──────────────────────────────
//
// Bedrock ConverseStream returns `application/vnd.amazon.eventstream`
// binary format. Each message is:
//   [total_byte_length:  u32 BE]
//   [headers_byte_length: u32 BE]
//   [prelude_crc:         u32 BE]
//   [headers:             variable]
//   [payload:             variable]
//   [message_crc:         u32 BE]
//
// We skip CRC validation since the connection is already TLS-protected.

/// Parse a single event-stream message from a byte buffer.
/// Returns `(event_type, payload_bytes, total_consumed)` or None if not enough data.
fn parse_event_stream_message(buf: &[u8]) -> Option<(String, Vec<u8>, usize)> {
    // Minimum message: 4 (total_len) + 4 (header_len) + 4 (prelude_crc) + 4 (message_crc) = 16
    if buf.len() < 16 {
        return None;
    }

    let total_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if buf.len() < total_len {
        return None;
    }

    let headers_len = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
    // prelude_crc is at bytes 8..12, skip it
    let headers_start = 12;
    let headers_end = headers_start + headers_len;
    let payload_start = headers_end;
    let payload_end = total_len - 4; // 4 bytes for message_crc

    // Parse headers to find :event-type
    let mut event_type = String::new();
    let mut pos = headers_start;
    while pos < headers_end {
        if pos >= buf.len() {
            break;
        }
        let name_len = buf[pos] as usize;
        pos += 1;
        if pos + name_len > buf.len() {
            break;
        }
        let name = String::from_utf8_lossy(&buf[pos..pos + name_len]).to_string();
        pos += name_len;
        if pos >= buf.len() {
            break;
        }
        let value_type = buf[pos];
        pos += 1;
        match value_type {
            7 => {
                // String type
                if pos + 2 > buf.len() {
                    break;
                }
                let val_len = u16::from_be_bytes([buf[pos], buf[pos + 1]]) as usize;
                pos += 2;
                if pos + val_len > buf.len() {
                    break;
                }
                let value = String::from_utf8_lossy(&buf[pos..pos + val_len]).to_string();
                pos += val_len;
                if name == ":event-type" {
                    event_type = value;
                }
            }
            _ => {
                // Skip other header types. Most are fixed-size or have length prefixes.
                // For safety, just break if we hit an unknown type.
                break;
            }
        }
    }

    let payload = if payload_start < payload_end && payload_end <= buf.len() {
        buf[payload_start..payload_end].to_vec()
    } else {
        Vec::new()
    };

    Some((event_type, payload, total_len))
}

/// Bedrock converse-stream event payloads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContentBlockDelta {
    #[allow(dead_code)]
    content_block_index: Option<u32>,
    delta: DeltaContent,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeltaContent {
    #[serde(default)]
    text: Option<String>,
}

/// Convert a Bedrock converse-stream byte response into a stream of `StreamChunk`s.
fn bedrock_event_stream_to_chunks(
    response: reqwest::Response,
    count_tokens: bool,
) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamChunk>>(100);

    tokio::spawn(async move {
        let mut buffer = Vec::new();
        let mut bytes_stream = response.bytes_stream();

        while let Some(item) = bytes_stream.next().await {
            match item {
                Ok(bytes) => {
                    buffer.extend_from_slice(&bytes);

                    // Try to parse complete messages from the buffer
                    while let Some((event_type, payload, consumed)) =
                        parse_event_stream_message(&buffer)
                    {
                        buffer.drain(..consumed);

                        match event_type.as_str() {
                            "contentBlockDelta" => {
                                if let Ok(delta) =
                                    serde_json::from_slice::<ContentBlockDelta>(&payload)
                                {
                                    if let Some(text) = delta.delta.text {
                                        if !text.is_empty() {
                                            let mut chunk = StreamChunk::delta(text);
                                            if count_tokens {
                                                chunk = chunk.with_token_estimate();
                                            }
                                            if tx.send(Ok(chunk)).await.is_err() {
                                                return;
                                            }
                                        }
                                    }
                                }
                            }
                            "messageStop" | "metadata" | "messageStart" | "contentBlockStart"
                            | "contentBlockStop" => {
                                // Informational or final — skip (final chunk sent after loop)
                            }
                            other if other.contains("Exception") || other.contains("Error") => {
                                let msg = String::from_utf8_lossy(&payload).to_string();
                                let _ = tx
                                    .send(Err(StreamError::Provider(format!(
                                        "Bedrock stream error ({other}): {msg}"
                                    ))))
                                    .await;
                                return;
                            }
                            _ => {} // Unknown event type, skip
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(StreamError::Http(e))).await;
                    break;
                }
            }
        }

        // Send final chunk
        let _ = tx.send(Ok(StreamChunk::final_chunk())).await;
    });

    stream::unfold(rx, |mut rx| async {
        rx.recv().await.map(|chunk| (chunk, rx))
    })
    .boxed()
}

// ── Provider trait implementation ───────────────────────────────

#[async_trait]
impl Provider for BedrockProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: true,
            vision: true,
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
        temperature: f64,
    ) -> anyhow::Result<String> {
        let credentials = self.get_credentials().await?;

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

        let request = ConverseRequest {
            system,
            messages: vec![ConverseMessage {
                role: "user".to_string(),
                content: Self::parse_user_content_blocks(message),
            }],
            inference_config: Some(InferenceConfig {
                max_tokens: DEFAULT_MAX_TOKENS,
                temperature,
            }),
            tool_config: None,
        };

        let response = self
            .send_converse_request(&credentials, model, &request)
            .await?;

        Self::parse_converse_response(response)
            .text
            .ok_or_else(|| anyhow::anyhow!("No response from Bedrock"))
    }

    async fn chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ProviderChatResponse> {
        let credentials = self.get_credentials().await?;

        let (system_blocks, mut converse_messages) = Self::convert_messages(request.messages);

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
        if Self::should_cache_conversation(request.messages) {
            if let Some(last_msg) = converse_messages.last_mut() {
                last_msg
                    .content
                    .push(ContentBlock::CachePointBlock(CachePointWrapper {
                        cache_point: CachePoint::default_cache(),
                    }));
            }
        }

        let tool_config = Self::convert_tools_to_converse(request.tools);

        let converse_request = ConverseRequest {
            system,
            messages: converse_messages,
            inference_config: Some(InferenceConfig {
                max_tokens: DEFAULT_MAX_TOKENS,
                temperature,
            }),
            tool_config,
        };

        let response = self
            .send_converse_request(&credentials, model, &converse_request)
            .await?;

        Ok(Self::parse_converse_response(response))
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn stream_chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
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

        let request = ConverseRequest {
            system,
            messages: vec![ConverseMessage {
                role: "user".to_string(),
                content: Self::parse_user_content_blocks(message),
            }],
            inference_config: Some(InferenceConfig {
                max_tokens: DEFAULT_MAX_TOKENS,
                temperature,
            }),
            tool_config: None,
        };

        let cred_cache = self.credentials.inner.clone();
        let model = model.to_string();
        let count_tokens = options.count_tokens;
        let client = self.http_client();

        // We need to send the request asynchronously, then convert the response to a stream.
        // Use a channel to bridge the async setup with the streaming response.
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamChunk>>(100);

        tokio::spawn(async move {
            // Resolve credentials inside the async context so we get
            // TTL-validated, auto-refreshing credentials (not stale sync cache).
            let cred_handle = CachedCredentials { inner: cred_cache };
            let credentials = match cred_handle.get().await {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx
                        .send(Err(StreamError::Provider(format!(
                            "AWS Bedrock credentials not available: {e}"
                        ))))
                        .await;
                    return;
                }
            };

            let payload = match serde_json::to_vec(&request) {
                Ok(p) => p,
                Err(e) => {
                    let _ = tx
                        .send(Err(StreamError::Provider(format!(
                            "Failed to serialize request: {e}"
                        ))))
                        .await;
                    return;
                }
            };

            let url = BedrockProvider::stream_endpoint_url(&credentials.region, &model);
            let canonical_uri = BedrockProvider::stream_canonical_uri(&model);
            let now = chrono::Utc::now();
            let host = credentials.host();
            let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();

            let mut headers_to_sign = vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("host".to_string(), host),
                ("x-amz-date".to_string(), amz_date.clone()),
            ];
            if let Some(ref token) = credentials.session_token {
                headers_to_sign.push(("x-amz-security-token".to_string(), token.clone()));
            }
            headers_to_sign.sort_by(|a, b| a.0.cmp(&b.0));

            let authorization = build_authorization_header(
                &credentials,
                "POST",
                &canonical_uri,
                "",
                &headers_to_sign,
                &payload,
                &now,
            );

            let mut req = client
                .post(&url)
                .header("content-type", "application/json")
                .header("x-amz-date", &amz_date)
                .header("authorization", &authorization);

            if let Some(ref token) = credentials.session_token {
                req = req.header("x-amz-security-token", token);
            }

            let response = match req.body(payload).send().await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Err(StreamError::Http(e))).await;
                    return;
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "unknown error".to_string());
                let sanitized = super::sanitize_api_error(&body);
                let _ = tx
                    .send(Err(StreamError::Provider(format!(
                        "Bedrock stream request failed ({status}): {sanitized}"
                    ))))
                    .await;
                return;
            }

            // Parse the binary event stream
            let mut buffer = Vec::new();
            let mut bytes_stream = response.bytes_stream();

            while let Some(item) = bytes_stream.next().await {
                match item {
                    Ok(bytes) => {
                        buffer.extend_from_slice(&bytes);

                        while let Some((event_type, payload_bytes, consumed)) =
                            parse_event_stream_message(&buffer)
                        {
                            buffer.drain(..consumed);

                            match event_type.as_str() {
                                "contentBlockDelta" => {
                                    if let Ok(delta) =
                                        serde_json::from_slice::<ContentBlockDelta>(&payload_bytes)
                                    {
                                        if let Some(text) = delta.delta.text {
                                            if !text.is_empty() {
                                                let mut chunk = StreamChunk::delta(text);
                                                if count_tokens {
                                                    chunk = chunk.with_token_estimate();
                                                }
                                                if tx.send(Ok(chunk)).await.is_err() {
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                }
                                other if other.contains("Exception") || other.contains("Error") => {
                                    let msg = String::from_utf8_lossy(&payload_bytes).to_string();
                                    let _ = tx
                                        .send(Err(StreamError::Provider(format!(
                                            "Bedrock stream error ({other}): {msg}"
                                        ))))
                                        .await;
                                    return;
                                }
                                _ => {} // messageStart, contentBlockStart, contentBlockStop, messageStop, metadata — skip
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(StreamError::Http(e))).await;
                        break;
                    }
                }
            }

            let _ = tx.send(Ok(StreamChunk::final_chunk())).await;
        });

        stream::unfold(rx, |mut rx| async {
            rx.recv().await.map(|chunk| (chunk, rx))
        })
        .boxed()
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        if let Ok(creds) = self.get_credentials().await {
            let url = format!("https://{ENDPOINT_PREFIX}.{}.amazonaws.com/", creds.region);
            let _ = self.http_client().get(&url).send().await;
        }
        Ok(())
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::traits::ChatMessage;

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
        };
        assert_eq!(creds.host(), "bedrock-runtime.us-west-2.amazonaws.com");
    }

    // ── Provider construction tests ─────────────────────────────

    #[test]
    fn creates_without_credentials() {
        // Provider should construct even without env vars.
        let _provider = BedrockProvider::new();
    }

    #[tokio::test]
    async fn chat_fails_without_credentials() {
        let provider = BedrockProvider {
            credentials: CachedCredentials::new(None),
        };
        let result = provider
            .chat_with_system(None, "hello", "anthropic.claude-sonnet-4-6", 0.7)
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        let lower = err.to_lowercase();
        assert!(
            err.contains("credentials not set")
                || err.contains("169.254.169.254")
                || lower.contains("credential")
                || lower.contains("not authorized")
                || lower.contains("forbidden")
                || lower.contains("builder error")
                || lower.contains("builder"),
            "Expected missing-credentials style error, got: {err}"
        );
    }

    // ── Endpoint URL tests ──────────────────────────────────────

    #[test]
    fn endpoint_url_formats_correctly() {
        let url = BedrockProvider::endpoint_url("us-east-1", "anthropic.claude-sonnet-4-6");
        assert_eq!(
            url,
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude-sonnet-4-6/converse"
        );
    }

    #[test]
    fn endpoint_url_keeps_raw_colon() {
        // Endpoint URL uses raw colon so reqwest sends `:` on the wire.
        let url =
            BedrockProvider::endpoint_url("us-west-2", "anthropic.claude-3-5-haiku-20241022-v1:0");
        assert!(url.contains("/model/anthropic.claude-3-5-haiku-20241022-v1:0/converse"));
    }

    #[test]
    fn canonical_uri_encodes_colon() {
        // Canonical URI must encode `:` as `%3A` for SigV4 signing.
        let uri = BedrockProvider::canonical_uri("anthropic.claude-3-5-haiku-20241022-v1:0");
        assert_eq!(
            uri,
            "/model/anthropic.claude-3-5-haiku-20241022-v1%3A0/converse"
        );
    }

    #[test]
    fn canonical_uri_no_colon_unchanged() {
        let uri = BedrockProvider::canonical_uri("anthropic.claude-sonnet-4-6");
        assert_eq!(uri, "/model/anthropic.claude-sonnet-4-6/converse");
    }

    // ── Message conversion tests ────────────────────────────────

    #[test]
    fn convert_messages_system_extracted() {
        let messages = vec![
            ChatMessage::system("You are helpful"),
            ChatMessage::user("Hello"),
        ];
        let (system, msgs) = BedrockProvider::convert_messages(&messages);
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
        let (system, msgs) = BedrockProvider::convert_messages(&messages);
        assert!(system.is_none());
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].role, "assistant");
    }

    #[test]
    fn convert_messages_tool_role_to_tool_result() {
        let tool_json = r#"{"tool_call_id": "call_123", "content": "Result data"}"#;
        let messages = vec![ChatMessage::tool(tool_json)];
        let (_, msgs) = BedrockProvider::convert_messages(&messages);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
        assert!(matches!(msgs[0].content[0], ContentBlock::ToolResult(_)));
    }

    #[test]
    fn convert_messages_assistant_tool_calls_parsed() {
        let tool_call_json = r#"{"content": "Let me check", "tool_calls": [{"id": "call_1", "name": "shell", "arguments": "{\"command\":\"ls\"}"}]}"#;
        let messages = vec![ChatMessage::assistant(tool_call_json)];
        let (_, msgs) = BedrockProvider::convert_messages(&messages);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "assistant");
        assert_eq!(msgs[0].content.len(), 2);
        assert!(matches!(msgs[0].content[0], ContentBlock::Text(_)));
        assert!(matches!(msgs[0].content[1], ContentBlock::ToolUse(_)));
    }

    #[test]
    fn convert_messages_plain_assistant_text() {
        let messages = vec![ChatMessage::assistant("Just text")];
        let (_, msgs) = BedrockProvider::convert_messages(&messages);
        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0].content[0], ContentBlock::Text(_)));
    }

    // ── Cache tests ─────────────────────────────────────────────

    #[test]
    fn should_cache_system_small_prompt() {
        assert!(!BedrockProvider::should_cache_system("Short prompt"));
    }

    #[test]
    fn should_cache_system_large_prompt() {
        let large = "a".repeat(3073);
        assert!(BedrockProvider::should_cache_system(&large));
    }

    #[test]
    fn should_cache_system_boundary() {
        assert!(!BedrockProvider::should_cache_system(&"a".repeat(3072)));
        assert!(BedrockProvider::should_cache_system(&"a".repeat(3073)));
    }

    #[test]
    fn should_cache_conversation_short() {
        let messages = vec![
            ChatMessage::system("System"),
            ChatMessage::user("Hello"),
            ChatMessage::assistant("Hi"),
        ];
        assert!(!BedrockProvider::should_cache_conversation(&messages));
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
        assert!(BedrockProvider::should_cache_conversation(&messages));
    }

    // ── Tool conversion tests ───────────────────────────────────

    #[test]
    fn convert_tools_to_converse_formats_correctly() {
        let tools = vec![ToolSpec {
            name: "shell".to_string(),
            description: "Run commands".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
        }];
        let config = BedrockProvider::convert_tools_to_converse(Some(&tools));
        assert!(config.is_some());
        let config = config.unwrap();
        assert_eq!(config.tools.len(), 1);
        assert_eq!(config.tools[0].tool_spec.name, "shell");
    }

    #[test]
    fn convert_tools_to_converse_empty_returns_none() {
        assert!(BedrockProvider::convert_tools_to_converse(Some(&[])).is_none());
        assert!(BedrockProvider::convert_tools_to_converse(None).is_none());
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
                temperature: 0.7,
            }),
            tool_config: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("system"));
        assert!(json.contains("Hello"));
        assert!(json.contains("maxTokens"));
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
        let parsed = BedrockProvider::parse_converse_response(resp);
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
        let parsed = BedrockProvider::parse_converse_response(resp);
        assert!(parsed.text.is_none());
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "shell");
        assert_eq!(parsed.tool_calls[0].id, "call_1");
    }

    #[test]
    fn converse_response_empty_output() {
        let json = r#"{"output": null, "stopReason": null}"#;
        let resp: ConverseResponse = serde_json::from_str(json).unwrap();
        let parsed = BedrockProvider::parse_converse_response(resp);
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
        let provider = BedrockProvider {
            credentials: CachedCredentials::new(None),
        };
        let result = provider.warmup().await;
        assert!(result.is_ok());
    }

    #[test]
    fn capabilities_reports_native_tool_calling() {
        let provider = BedrockProvider {
            credentials: CachedCredentials::new(None),
        };
        let caps = provider.capabilities();
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
        let (_, msgs) = BedrockProvider::convert_messages(&messages);
        let tool_msg = &msgs[2];
        assert_eq!(tool_msg.role, "user");
        assert!(
            matches!(&tool_msg.content[0], ContentBlock::ToolResult(_)),
            "Expected ToolResult block, got {:?}",
            tool_msg.content[0]
        );
    }

    // ── Streaming tests ──────────────────────────────────────────

    #[test]
    fn supports_streaming_returns_true() {
        let provider = BedrockProvider {
            credentials: CachedCredentials::new(None),
        };
        assert!(provider.supports_streaming());
    }

    #[test]
    fn stream_endpoint_url_formats_correctly() {
        let url = BedrockProvider::stream_endpoint_url("us-east-1", "anthropic.claude-sonnet-4-6");
        assert_eq!(
            url,
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude-sonnet-4-6/converse-stream"
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
        let (_, msgs) = BedrockProvider::convert_messages(&messages);
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
        let (_, msgs) = BedrockProvider::convert_messages(&messages);
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
            BedrockProvider::extract_tool_call_id(r#"{"tool_call_id":"a"}"#),
            Some("a".to_string())
        );
        assert_eq!(
            BedrockProvider::extract_tool_call_id(r#"{"tool_use_id":"b"}"#),
            Some("b".to_string())
        );
        assert_eq!(
            BedrockProvider::extract_tool_call_id(r#"{"toolUseId":"c"}"#),
            Some("c".to_string())
        );
        assert_eq!(
            BedrockProvider::extract_tool_call_id("not json at all"),
            None
        );
    }

    #[test]
    fn stream_canonical_uri_encodes_colon() {
        let uri = BedrockProvider::stream_canonical_uri("anthropic.claude-3-5-haiku-20241022-v1:0");
        assert_eq!(
            uri,
            "/model/anthropic.claude-3-5-haiku-20241022-v1%3A0/converse-stream"
        );
    }

    #[test]
    fn parse_tool_result_accepts_alternate_id_fields() {
        let msg =
            BedrockProvider::parse_tool_result_message(r#"{"tool_use_id":"x","content":"ok"}"#);
        assert!(msg.is_some());
        if let ContentBlock::ToolResult(ref wrapper) = msg.unwrap().content[0] {
            assert_eq!(wrapper.tool_result.tool_use_id, "x");
        } else {
            panic!("Expected ToolResult");
        }
    }

    #[test]
    fn stream_canonical_uri_no_colon() {
        let uri = BedrockProvider::stream_canonical_uri("anthropic.claude-sonnet-4-6");
        assert_eq!(uri, "/model/anthropic.claude-sonnet-4-6/converse-stream");
    }

    // ── Event-stream parser tests ────────────────────────────────

    /// Helper: build a minimal AWS event-stream message with a string `:event-type` header.
    #[allow(clippy::cast_possible_truncation)]
    fn build_event_stream_message(event_type: &str, payload: &[u8]) -> Vec<u8> {
        // Header: `:event-type` as string (type 7)
        let header_name = b":event-type";
        let header_name_len = header_name.len() as u8;
        let event_type_bytes = event_type.as_bytes();
        let event_type_len = event_type_bytes.len() as u16;

        // Header bytes: 1 (name_len) + name + 1 (type=7) + 2 (val_len) + val
        let headers_len = 1 + header_name.len() + 1 + 2 + event_type_bytes.len();
        // Total: 4 (total_len) + 4 (headers_len) + 4 (prelude_crc) + headers + payload + 4 (message_crc)
        let total_len = 12 + headers_len + payload.len() + 4;

        let mut msg = Vec::with_capacity(total_len);
        msg.extend_from_slice(&(total_len as u32).to_be_bytes());
        msg.extend_from_slice(&(headers_len as u32).to_be_bytes());
        msg.extend_from_slice(&0u32.to_be_bytes()); // prelude_crc (skipped)

        // Write header
        msg.push(header_name_len);
        msg.extend_from_slice(header_name);
        msg.push(7); // string type
        msg.extend_from_slice(&event_type_len.to_be_bytes());
        msg.extend_from_slice(event_type_bytes);

        // Write payload
        msg.extend_from_slice(payload);

        // Write message CRC (skipped, just zeros)
        msg.extend_from_slice(&0u32.to_be_bytes());

        msg
    }

    #[test]
    fn parse_event_stream_message_content_block_delta() {
        let payload = br#"{"contentBlockIndex":0,"delta":{"text":"Hello"}}"#;
        let msg = build_event_stream_message("contentBlockDelta", payload);

        let result = parse_event_stream_message(&msg);
        assert!(result.is_some());
        let (event_type, parsed_payload, consumed) = result.unwrap();
        assert_eq!(event_type, "contentBlockDelta");
        assert_eq!(consumed, msg.len());

        let delta: ContentBlockDelta = serde_json::from_slice(&parsed_payload).unwrap();
        assert_eq!(delta.delta.text.as_deref(), Some("Hello"));
    }

    #[test]
    fn parse_event_stream_message_stop() {
        let payload = br#"{"stopReason":"end_turn"}"#;
        let msg = build_event_stream_message("messageStop", payload);

        let result = parse_event_stream_message(&msg);
        assert!(result.is_some());
        let (event_type, _, _) = result.unwrap();
        assert_eq!(event_type, "messageStop");
    }

    #[test]
    fn parse_event_stream_message_insufficient_data() {
        // Only 10 bytes — not enough for even the minimum 16-byte message
        let buf = vec![0u8; 10];
        assert!(parse_event_stream_message(&buf).is_none());
    }

    #[test]
    fn parse_event_stream_message_incomplete_message() {
        let payload = br#"{"text":"Hi"}"#;
        let msg = build_event_stream_message("contentBlockDelta", payload);

        // Truncate to simulate incomplete data
        let truncated = &msg[..msg.len() - 5];
        assert!(parse_event_stream_message(truncated).is_none());
    }

    #[test]
    fn parse_event_stream_multiple_messages() {
        let payload1 = br#"{"contentBlockIndex":0,"delta":{"text":"Hello"}}"#;
        let payload2 = br#"{"contentBlockIndex":0,"delta":{"text":" World"}}"#;
        let msg1 = build_event_stream_message("contentBlockDelta", payload1);
        let msg2 = build_event_stream_message("contentBlockDelta", payload2);

        let mut buf = Vec::new();
        buf.extend_from_slice(&msg1);
        buf.extend_from_slice(&msg2);

        // Parse first message
        let (event_type1, p1, consumed1) = parse_event_stream_message(&buf).unwrap();
        assert_eq!(event_type1, "contentBlockDelta");
        let delta1: ContentBlockDelta = serde_json::from_slice(&p1).unwrap();
        assert_eq!(delta1.delta.text.as_deref(), Some("Hello"));

        // Parse second message from remainder
        let (event_type2, p2, _) = parse_event_stream_message(&buf[consumed1..]).unwrap();
        assert_eq!(event_type2, "contentBlockDelta");
        let delta2: ContentBlockDelta = serde_json::from_slice(&p2).unwrap();
        assert_eq!(delta2.delta.text.as_deref(), Some(" World"));
    }

    #[test]
    fn content_block_delta_deserializes() {
        let json = r#"{"contentBlockIndex":0,"delta":{"text":"Hello from Bedrock"}}"#;
        let delta: ContentBlockDelta = serde_json::from_str(json).unwrap();
        assert_eq!(delta.content_block_index, Some(0));
        assert_eq!(delta.delta.text.as_deref(), Some("Hello from Bedrock"));
    }

    #[test]
    fn content_block_delta_empty_text() {
        let json = r#"{"contentBlockIndex":0,"delta":{}}"#;
        let delta: ContentBlockDelta = serde_json::from_str(json).unwrap();
        assert!(delta.delta.text.is_none());
    }
}
