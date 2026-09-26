//! Generic OpenAI-compatible model_provider.
//! Most LLM APIs follow the same `/v1/chat/completions` format.
//! This module provides a single implementation that works for all of them.

use crate::auth::AuthService;
use crate::multimodal;
use crate::openai::{NativeToolFunctionSpec, NativeToolSpec};
use crate::opencode_session::OPENCODE_SESSION_HEADER;
use crate::stream_guard::AbortOnDrop;
use crate::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    ModelProvider, StreamChunk, StreamError, StreamEvent, StreamOptions, StreamResult,
    ToolCall as ProviderToolCall,
};
use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use reqwest::{
    Client, ClientBuilder,
    header::{HeaderMap, HeaderValue, USER_AGENT},
};
use serde::{Deserialize, Serialize};
use zeroclaw_config::schema::{CacheTtl, ToolResultImagePolicy};

const TOOL_RESULT_IMAGE_OMITTED_NOTICE: &str = "[tool-result image omitted by provider policy]";

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
    canonical_base_url: Option<&'static str>,
    pub credential: Option<String>,
    auth_service: Option<AuthService>,
    auth_model_provider: Option<String>,
    auth_profile_override: Option<String>,
    pub auth_header: AuthStyle,
    supports_vision: bool,
    tool_result_image_policy: ToolResultImagePolicy,
    /// Operator `[multimodal]` policy for this provider's image-marker
    /// expansion pass. Resolved once at build time.
    multimodal: zeroclaw_config::schema::MultimodalConfig,
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
    /// When true, forward the configured reasoning effort to any model,
    /// bypassing the OpenAI-reasoning-family name filter. The filter exists
    /// because some backends reject unknown request params; operators enable
    /// this only for backends they have verified accept `reasoning_effort`
    /// (GLM/Kimi/DeepSeek/Qwen-style reasoners behind OpenAI-compatible
    /// gateways commonly do).
    reasoning_effort_passthrough: bool,
    /// Whether stored assistant reasoning should be replayed on outbound
    /// assistant history messages. Some providers reject reasoning fields as
    /// input even though they may return them in responses.
    replay_assistant_reasoning: bool,
    /// Whether Anthropic prompt-cache breakpoints should be injected into
    /// outbound request bodies (system prompt; rolling last message).
    /// Drives `capabilities().prompt_caching` and the request-build path.
    cache_passthrough: bool,
    /// Cache entry lifetime carried by every breakpoint injected behind
    /// `cache_passthrough`. One TTL per request by design. Inert without
    /// passthrough: no markers are placed, so nothing carries a TTL.
    cache_ttl: Option<CacheTtl>,
    /// When true, forward runtime-supplied native thinking params as an
    /// Anthropic-shaped `thinking` object in request bodies and normalize
    /// gateway thinking responses for replay. Requires a gateway that
    /// translates between OpenAI Chat Completions and the Anthropic API;
    /// a non-translating upstream rejects the injected object with HTTP 400.
    thinking_passthrough: bool,
    /// Custom API path suffix (e.g. "/v2/generate").
    /// When set, overrides the default `/chat/completions` path detection.
    api_path: Option<String>,
    /// Maximum output tokens to include in API requests.
    max_tokens: Option<u32>,
    /// models.dev catalog key for this model_provider (e.g. "xai").
    /// When set, `list_models` fetches from the models.dev catalog.
    models_dev_key: Option<String>,
    openrouter_vendor_prefix: Option<String>,
    local_model_tool_sanitize: bool,
    /// Some OpenAI-compatible local servers, such as Ollama, expose `/models`
    /// without authentication. Keep the default credential-gated for hosted
    /// providers so missing credentials still fall through to catalog sources.
    /// When `true`, the `/models` endpoint is treated as publicly accessible.
    public_model_listing: bool,
    /// Raw PEM bytes of a custom CA certificate for TLS connections.
    /// Loaded from disk once at construction; not refreshed across config reloads.
    tls_ca_cert_pem: Option<Vec<u8>>,
    /// Extra JSON fields merged into every API request body.
    extra_body: Option<serde_json::Value>,
    /// Memoized cleaned tool schemas: each registered schema is cleaned once
    /// per strategy per provider instance and then `Arc`-shared into every
    /// request body instead of being deep-copied per request. `Arc` so
    /// provider clones (e.g. the streaming path's owned copy) share one
    /// memo. Paths that rebuild the provider per call (e.g. the
    /// per-iteration vision route) start it empty each time.
    schema_cache: std::sync::Arc<zeroclaw_api::schema::SchemaCleanCache>,
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

/// Sanitize a tool-call `arguments` string before it is re-serialized into an
/// outbound OpenAI-compatible chat-completions request.
///
/// Several strict upstream providers (Cohere, OpenInference, Nvidia …,
/// surfaced most often through OpenRouter) reject requests where
/// `tool_calls[].function.arguments` is not well-formed JSON. Smaller /
/// reasoning models sometimes emit a malformed arguments string; when that
/// happens the whole turn fails with HTTP 400 and the user receives the
/// generic fallback instead of the agent's response.
///
/// Contract:
/// - empty / whitespace-only → `"{}"` (every upstream accepts this)
/// - valid JSON → returned unchanged
/// - invalid JSON → WARN-logged with **safe metadata only** (function name,
///   payload length, stable error key), then `"{}"`. The raw arguments
///   string is **never** recorded, because tool-call arguments can contain
///   commands, URLs, credentials, file paths, or user content and WARN
///   events enter the broadcast and rolling-persistence path regardless of
///   the tool/LLM content-capture policy.
///
/// This is the single source of truth for the tool-call arguments
/// normalization contract. The streaming accumulator's
/// `StreamToolCallAccumulator::into_provider_tool_call` and all typed
/// providers' outbound `convert_messages` paths route through here.
pub(crate) fn sanitize_tool_arguments(function_name: &str, arguments: &str) -> String {
    if arguments.trim().is_empty() {
        return "{}".to_string();
    }
    match serde_json::from_str::<serde_json::Value>(arguments) {
        Ok(serde_json::Value::Object(_)) => return arguments.to_string(),
        Ok(_non_object) => {
            // Accept only JSON objects; null, arrays, strings, numbers, and
            // booleans do not satisfy a strict-provider function-arguments
            // contract (reported by Cohere, tracked by OpenRouter's
            // auto-exacto validator).
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "function": function_name,
                        "payload_len": arguments.len(),
                        "error_key": "tool_args_not_object",
                    })),
                "Non-object tool-call arguments being sent to strict upstream provider, dropping to empty object"
            );
            return "{}".to_string();
        }
        Err(_) => {}
    }
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({
                "function": function_name,
                "payload_len": arguments.len(),
                "error_key": "tool_args_invalid_json",
            })),
        "Invalid JSON in tool-call arguments being sent to upstream provider, dropping to empty object"
    );
    "{}".to_string()
}

/// Generate a Zhipu JWT from an `id.secret` API key.
/// Returns `Authorization: Bearer <jwt>` value. Token is valid for 3.5 minutes.
fn zhipu_jwt_bearer(credential: &str) -> Result<String, String> {
    let (id, secret) = credential
        .split_once('.')
        .ok_or_else(|| "Zhipu API key must be in 'id.secret' format".to_string())?;
    if id.is_empty() || secret.is_empty() {
        return Err("Zhipu API key must contain non-empty id and secret components".to_string());
    }

    #[allow(clippy::cast_possible_truncation)] // millis won't exceed u64 until year 584 million
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis() as u64;
    let exp_ms = now_ms + 210_000; // 3.5 minutes

    // Header: {"alg":"HS256","typ":"JWT","sign_type":"SIGN"}
    let header_b64 = base64url_no_pad(br#"{"alg":"HS256","typ":"JWT","sign_type":"SIGN"}"#);
    let payload = serde_json::json!({
        "api_key": id,
        "exp": exp_ms,
        "timestamp": now_ms,
    })
    .to_string();
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
/// When `credential` is `None` (e.g. local LLM servers that require no API key),
/// the request is returned unchanged -- no auth header is added.
///
/// Within the OpenAI-compatible family builder this is where a stored
/// credential becomes an outbound header, and the requests built here --
/// chat, model listing, and context-window discovery
/// ([`crate::fetch_context_window`]) -- share it. That is what keeps a family
/// whose credential must be transformed before it is usable
/// (`AuthStyle::ZhipuJwt` mints a short-lived JWT from the stored `id.secret`)
/// from having one of those paths transform it while another sends it raw.
///
/// Scoped deliberately: providers implemented outside this module (Anthropic,
/// Gemini, Bedrock, …) authenticate their own way and make no claim here.
///
/// Fails closed. `AuthStyle::ZhipuJwt` mints a short-lived JWT from a stored
/// `id.secret`; when that minting fails the request is *not* built. The
/// alternative — attaching no `Authorization` header and sending anyway —
/// produces a request that can only ever be rejected upstream, and reports
/// the refusal as whatever status the provider happens to return rather than
/// as the local credential problem it is.
///
/// `provider` names the caller for the refusal record. Both the request path
/// and context-window discovery pass it explicitly rather than relying on an
/// ambient span, because discovery builds its probe outside any per-provider
/// span.
pub(crate) fn apply_auth_to_request(
    req: reqwest::RequestBuilder,
    style: &AuthStyle,
    credential: Option<&str>,
    provider: &str,
) -> anyhow::Result<reqwest::RequestBuilder> {
    let credential = match credential {
        Some(c) => c,
        None => return Ok(req),
    };
    Ok(match style {
        AuthStyle::Bearer => req.header("Authorization", format!("Bearer {credential}")),
        AuthStyle::XApiKey => req.header("x-api-key", credential),
        AuthStyle::Custom(header) => req.header(header, credential),
        AuthStyle::ZhipuJwt => req.header(
            "Authorization",
            zhipu_jwt_bearer_or_refuse(credential, provider)?,
        ),
    })
}

/// Mint the `Authorization` value for [`AuthStyle::ZhipuJwt`], or refuse.
///
/// Split out so the refusal is one place: the reason is logged against the
/// provider that owns the credential and turned into an operator-readable
/// error whose text is built only from the static failure reason, never from
/// the credential itself.
fn zhipu_jwt_bearer_or_refuse(credential: &str, provider: &str) -> anyhow::Result<String> {
    zhipu_jwt_bearer(credential).map_err(|reason| {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "model_provider": provider,
                    "auth_style": "zhipu_jwt",
                    "reason": &reason,
                    "error_key": "provider_credential_unusable",
                })),
            "compatible: stored credential could not be converted into a request token; \
             refusing to send the request"
        );
        anyhow::Error::msg(format!(
            "{provider}: stored credential cannot be converted into a request token \
             ({reason}); no request was sent"
        ))
    })
}

fn structured_api_error_message(value: &serde_json::Value) -> Option<String> {
    let object = value.as_object()?;

    let nested_message = object.get("error").and_then(|error| match error {
        serde_json::Value::Object(_) => structured_api_error_message(error),
        serde_json::Value::String(error) => serde_json::from_str(error)
            .ok()
            .and_then(|nested| structured_api_error_message(&nested)),
        _ => None,
    });
    if let Some(message) = nested_message {
        return Some(message);
    }

    for key in ["message", "detail"] {
        if let Some(message) = object
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|message| !message.is_empty())
        {
            return Some(message.to_string());
        }
    }

    object
        .get("error")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(str::to_string)
}

fn streaming_api_error(status: reqwest::StatusCode, body: &str) -> StreamError {
    let message = serde_json::from_str(body)
        .ok()
        .and_then(|value| structured_api_error_message(&value));
    let sanitized = super::sanitize_api_error(message.as_deref().unwrap_or(body));
    StreamError::ModelProvider(format!("{status}: {sanitized}"))
}

/// Upper bound on a `/models` catalog response buffered before parsing. Real
/// catalogs run to at most a few hundred KB (hundreds of models with pricing),
/// so this leaves generous headroom while stopping a misbehaving or compromised
/// router from making the client buffer an unbounded body — a boundary that
/// matters most on the public, credential-free listing path a `PUBLIC_MODEL_LISTING`
/// family (ZeroRouter, Kilo, AtlasCloud) exposes.
pub(crate) const MAX_MODELS_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// Read a response body into memory, refusing anything past `max_bytes`: a
/// declared `Content-Length` over the cap fails fast, and a stream that grows
/// past it fails as the bytes arrive (so a lying or absent `Content-Length`
/// cannot get around the bound). Mirrors the bounded reader in `zeroclaw-channels`
/// so both behave identically, without taking a cross-crate dependency for it.
pub(crate) async fn read_body_capped(
    mut response: reqwest::Response,
    max_bytes: u64,
) -> anyhow::Result<Vec<u8>> {
    if let Some(content_length) = response.content_length()
        && content_length > max_bytes
    {
        anyhow::bail!(
            "response body content length {content_length} exceeds {max_bytes}-byte limit"
        );
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let next_len = u64::try_from(body.len())
            .unwrap_or(u64::MAX)
            .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if next_len > max_bytes {
            anyhow::bail!("response body exceeds {max_bytes}-byte limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Deserialize)]
pub(crate) struct ModelsResponse {
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

pub(crate) fn parse_model_ids_from_bytes(bytes: &[u8]) -> anyhow::Result<Vec<String>> {
    let body: ModelsResponse = serde_json::from_slice(bytes)?;
    Ok(normalize_model_ids(body))
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
            // OpenAI-compatible `/v1/models` has no context-window field.
            context_window: None,
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

/// Map a models.dev listing into `ModelInfo`, carrying through the catalog's
/// context window. A model the catalog gives no `limit.context` for stays
/// `None` — "unknown", never a stub value.
fn models_dev_to_model_info(
    models: Vec<(String, Option<usize>)>,
) -> Vec<zeroclaw_api::model_provider::ModelInfo> {
    use zeroclaw_api::model_provider::ModelInfo;
    models
        .into_iter()
        .map(|(id, context_window)| ModelInfo {
            id,
            pricing: None,
            context_window,
        })
        .collect()
}

/// Typed builder for [`OpenAiCompatibleModelProvider`].
///
/// `alias` (the config key this provider was constructed under) is the
/// only argument passed to [`OpenAiCompatibleModelProvider::builder`].
/// Every other field — including the semantically-required
/// `display_name` / `base_url` / `auth_style` — is set via a labelled
/// chain method so call sites read as prose instead of a comma-counted
/// tuple. `build()` panics if any of `display_name`, `base_url`, or
/// `auth_style` were omitted; there are no sensible defaults for those.
#[must_use]
pub struct OpenAiCompatibleBuilder {
    alias: String,
    name: Option<String>,
    base_url: Option<String>,
    canonical_base_url: Option<&'static str>,
    credential: Option<String>,
    auth_style: Option<AuthStyle>,
    supports_vision: bool,
    tool_result_image_policy: ToolResultImagePolicy,
    multimodal: zeroclaw_config::schema::MultimodalConfig,
    user_agent: Option<String>,
    /// Set via [`OpenAiCompatibleBuilder::merge_system_into_user`] — the
    /// combined "merge + drop native tool calling" preset. Distinct from
    /// [`OpenAiCompatibleBuilder::merge_system_into_user_preserving_native`]
    /// which keeps native tools on.
    merge_system_into_user: bool,
    /// Set via [`OpenAiCompatibleBuilder::merge_system_into_user_preserving_native`]
    /// to enable the merge behaviour without disabling native tool calling.
    merge_system_into_user_preserve_native: bool,
    /// Set to `Some(false)` by [`OpenAiCompatibleBuilder::without_native_tools`].
    /// `None` preserves the default derived from `merge_system_into_user`.
    native_tool_calling_override: Option<bool>,
    timeout_secs: Option<u64>,
    extra_headers: std::collections::HashMap<String, String>,
    reasoning_effort: Option<String>,
    /// Set to `true` by
    /// [`OpenAiCompatibleBuilder::with_reasoning_effort_passthrough`].
    /// Default `false` keeps the OpenAI-reasoning-family name filter in
    /// charge of which models receive `reasoning_effort`.
    reasoning_effort_passthrough: bool,
    /// Set to `Some(false)` by
    /// [`OpenAiCompatibleBuilder::without_assistant_reasoning_replay`]. `None`
    /// preserves the default (replay enabled).
    replay_assistant_reasoning_override: Option<bool>,
    /// Set by [`OpenAiCompatibleBuilder::with_cache_passthrough`]. Default
    /// `false`: requests and capability reporting are unchanged.
    cache_passthrough: bool,
    /// Set by [`OpenAiCompatibleBuilder::with_cache_ttl`]. Default `None`:
    /// breakpoints keep the 5-minute API default.
    cache_ttl: Option<CacheTtl>,
    /// Set to `true` by [`OpenAiCompatibleBuilder::with_thinking_passthrough`].
    /// Default `false` leaves requests and response handling unchanged.
    thinking_passthrough: bool,
    api_path: Option<String>,
    max_tokens: Option<u32>,
    models_dev_key: Option<String>,
    openrouter_vendor_prefix: Option<String>,
    local_model_tool_sanitize: bool,
    public_model_listing: bool,
    tls_ca_cert_path: Option<String>,
    extra_body: Option<serde_json::Value>,
    auth_model_provider: Option<String>,
    auth_service: Option<AuthService>,
    auth_profile_override: Option<String>,
}

impl OpenAiCompatibleBuilder {
    /// Human-readable display name (e.g. `"Groq"`, `"MiniMax"`). Surfaced
    /// in logs, `Attributable` output, and the onboarding UI. Required.
    pub fn display_name(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }

    /// Base URL for the provider's `/chat/completions` endpoint. Trailing
    /// slashes are stripped so callers need not care whether config
    /// supplied them. Required.
    pub fn base_url(mut self, base_url: &str) -> Self {
        self.base_url = Some(base_url.trim_end_matches('/').to_string());
        self
    }

    pub(crate) fn canonical_base_url(mut self, base_url: &'static str) -> Self {
        self.canonical_base_url = Some(base_url);
        self
    }

    /// Explicit API credential. `None` (the default) leaves this provider
    /// unauthenticated, which is how local LLM servers (Ollama,
    /// llama.cpp) are constructed. Whitespace-only inputs are normalized
    /// to `None` so a stray `Some("   ")` from config cannot produce a
    /// bogus `Bearer    ` header.
    pub fn credential(mut self, credential: Option<&str>) -> Self {
        self.credential = credential
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        self
    }

    /// How this provider expects the API key to be sent. Required.
    pub fn auth_style(mut self, style: AuthStyle) -> Self {
        self.auth_style = Some(style);
        self
    }

    /// Enable OpenAI-style multimodal (image) inputs on this provider.
    pub fn vision(mut self, supports_vision: bool) -> Self {
        self.supports_vision = supports_vision;
        self
    }

    /// Set the policy for image markers in native role=`tool` results.
    pub fn tool_result_image_policy(mut self, policy: ToolResultImagePolicy) -> Self {
        self.tool_result_image_policy = policy;
        self
    }

    /// Set the root `[multimodal]` policy used when expanding `[IMAGE:...]`
    /// markers into inline data URIs. Without this the provider falls back to
    /// library defaults and operator limits never reach the outbound request.
    pub fn multimodal(mut self, config: zeroclaw_config::schema::MultimodalConfig) -> Self {
        self.multimodal = config;
        self
    }

    /// Set a custom `User-Agent` header for outbound requests.
    ///
    /// Required by providers whose routing / policy stack keys off the UA
    /// string (for example Kimi Code).
    pub fn user_agent(mut self, user_agent: &str) -> Self {
        self.user_agent = Some(user_agent.to_string());
        self
    }

    /// For providers that reject `role: system` outright (e.g. MiniMax).
    /// Collects all system messages and prepends their content to the first
    /// user message; also disables native tool calling because such providers
    /// generally reject OpenAI-style `tools` payloads as well.
    ///
    /// Prefer [`OpenAiCompatibleBuilder::merge_system_into_user_preserving_native`]
    /// when you want the merge behaviour but still want native tool calling
    /// (e.g. Bedrock).
    pub fn merge_system_into_user(mut self) -> Self {
        self.merge_system_into_user = true;
        self
    }

    /// Merge all system messages into the first user message before sending,
    /// preserving native tool calling. Use when the upstream rejects
    /// `role: system` but still accepts OpenAI-style `tools` payloads (e.g.
    /// Bedrock's Anthropic pass-through).
    pub fn merge_system_into_user_preserving_native(mut self) -> Self {
        self.merge_system_into_user_preserve_native = true;
        self
    }

    /// Disable native tool calling, forcing prompt-guided tool use instead.
    pub fn without_native_tools(mut self) -> Self {
        self.native_tool_calling_override = Some(false);
        self
    }

    /// Override the HTTP request timeout for LLM API calls. Values of 0
    /// are ignored (the default 120 s is kept) so a stray `Some(0)` from
    /// config cannot silently disable the safety timeout.
    pub fn timeout_secs(mut self, timeout_secs: u64) -> Self {
        if timeout_secs > 0 {
            self.timeout_secs = Some(timeout_secs);
        }
        self
    }

    /// Set extra HTTP headers to include in all API requests.
    pub fn extra_headers(mut self, headers: std::collections::HashMap<String, String>) -> Self {
        self.extra_headers = headers;
        self
    }

    /// Set reasoning effort for GPT-5/Codex-compatible chat-completions APIs.
    pub fn reasoning_effort(mut self, reasoning_effort: Option<String>) -> Self {
        self.reasoning_effort = reasoning_effort;
        self
    }

    /// Forward the configured reasoning effort to every model on this
    /// provider, bypassing the OpenAI-reasoning-family name filter. The
    /// filter exists because some backends reject unknown request params;
    /// enable this only on backends verified to accept `reasoning_effort`.
    pub fn with_reasoning_effort_passthrough(mut self) -> Self {
        self.reasoning_effort_passthrough = true;
        self
    }

    /// Disable replay of stored assistant reasoning on outbound assistant
    /// history messages.
    pub fn without_assistant_reasoning_replay(mut self) -> Self {
        self.replay_assistant_reasoning_override = Some(false);
        self
    }

    /// Opt this provider into Anthropic prompt-cache passthrough: request
    /// bodies gain `cache_control` breakpoints (system prompt; rolling last
    /// message once the conversation has more than one non-system message),
    /// mirroring the native Anthropic provider's placement strategy, and
    /// gateway-reported cache usage is captured. The flag also reports
    /// `prompt_caching` in the provider capabilities.
    pub fn with_cache_passthrough(mut self) -> Self {
        self.cache_passthrough = true;
        self
    }

    /// Request the given cache entry lifetime on every breakpoint injected
    /// behind `cache_passthrough`. Effective only together with
    /// [`Self::with_cache_passthrough`]: without passthrough no breakpoints
    /// are placed, so the setting is inert (no parse-time warning — an
    /// operator may stage the value before switching passthrough on).
    /// Defaults to the 5-minute API default when unset.
    pub fn with_cache_ttl(mut self, cache_ttl: CacheTtl) -> Self {
        self.cache_ttl = Some(cache_ttl);
        self
    }

    /// Forward Anthropic extended thinking through this provider: inject the
    /// runtime thinking params as an Anthropic-shaped `thinking` request
    /// object and normalize gateway thinking responses for replay. Only
    /// meaningful for gateways that translate between OpenAI Chat
    /// Completions and the Anthropic API (e.g. LiteLLM); a non-translating
    /// upstream rejects the injected object with HTTP 400. Off by default.
    pub fn with_thinking_passthrough(mut self) -> Self {
        self.thinking_passthrough = true;
        self
    }

    /// Set a custom API path suffix for this model_provider.
    pub fn api_path(mut self, api_path: Option<String>) -> Self {
        self.api_path = api_path;
        self
    }

    /// Set the maximum output tokens for API requests.
    pub fn max_tokens(mut self, max_tokens: Option<u32>) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Set the models.dev catalog key for this model_provider.
    pub fn models_dev_key(mut self, key: &str) -> Self {
        self.models_dev_key = Some(key.to_string());
        self
    }

    /// Set the OpenRouter vendor prefix for this model_provider.
    pub fn openrouter_vendor_prefix(mut self, prefix: &str) -> Self {
        self.openrouter_vendor_prefix = Some(prefix.to_string());
        self
    }

    /// Opt into per-model conservative tool-schema sanitization.
    pub fn local_model_tool_sanitize(mut self) -> Self {
        self.local_model_tool_sanitize = true;
        self
    }

    /// Treat the `/models` endpoint as publicly accessible.
    pub fn public_model_listing(mut self) -> Self {
        self.public_model_listing = true;
        self
    }

    /// Path to a PEM-encoded custom CA certificate for TLS connections.
    /// The file is read once at [`Self::build`] time; failures are logged
    /// at WARN and TLS falls back to the system trust store.
    pub fn tls_ca_cert_path(mut self, path: &str) -> Self {
        self.tls_ca_cert_path = Some(path.to_string());
        self
    }

    /// Inject extra JSON fields into every API request body.
    pub fn extra_body(mut self, extra: serde_json::Value) -> Self {
        self.extra_body = Some(extra);
        self
    }

    /// Use a stored auth profile as a bearer credential when no explicit
    /// `api_key` was configured on this provider entry.
    pub fn auth_profile(
        mut self,
        model_provider: &str,
        auth_service: AuthService,
        profile_override: Option<String>,
    ) -> Self {
        self.auth_model_provider = Some(model_provider.to_string());
        self.auth_service = Some(auth_service);
        self.auth_profile_override = profile_override;
        self
    }

    /// Finalize the builder into a ready provider. Every optional construction
    /// value must be set on this builder; the returned provider has no
    /// post-construction mutators.
    ///
    /// # Panics
    /// Panics if [`Self::display_name`], [`Self::base_url`], or
    /// [`Self::auth_style`] was not called — those three fields carry no
    /// sensible default and every real call site sets them.
    pub fn build(self) -> OpenAiCompatibleModelProvider {
        let name = self
            .name
            .expect("OpenAiCompatibleBuilder: display_name() is required");
        let base_url = self
            .base_url
            .expect("OpenAiCompatibleBuilder: base_url() is required");
        let auth_style = self
            .auth_style
            .expect("OpenAiCompatibleBuilder: auth_style() is required");
        // Either merge preset can enable the shared merge behavior.
        let merge_system_into_user =
            self.merge_system_into_user || self.merge_system_into_user_preserve_native;
        // Default `native_tool_calling` is `!merge_system_into_user_disable_native`,
        // i.e. only the "combined preset" builder setter disables it. The
        // explicit `without_native_tools()` override wins if present.
        let native_tool_calling = self
            .native_tool_calling_override
            .unwrap_or(!self.merge_system_into_user);
        // Read the PEM bytes now so later HTTP clients incur no per-request I/O.
        // A read error is logged at WARN and TLS falls back to system roots —
        // preserving the established warning-and-fallback semantics.
        let tls_ca_cert_pem =
            self.tls_ca_cert_path
                .as_deref()
                .and_then(|path| match std::fs::read(path) {
                    Ok(bytes) => Some(bytes),
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(
                                ::serde_json::json!({"path": path, "error": format!("{}", e)})
                            ),
                            "Failed to read CA certificate file — TLS will use system roots"
                        );
                        None
                    }
                });
        OpenAiCompatibleModelProvider {
            alias: self.alias,
            name,
            base_url,
            canonical_base_url: self.canonical_base_url,
            credential: self.credential,
            auth_service: self.auth_service,
            auth_model_provider: self.auth_model_provider,
            auth_profile_override: self.auth_profile_override,
            auth_header: auth_style,
            supports_vision: self.supports_vision,
            tool_result_image_policy: self.tool_result_image_policy,
            multimodal: self.multimodal,
            user_agent: self.user_agent,
            native_tool_calling,
            merge_system_into_user,
            timeout_secs: self.timeout_secs.unwrap_or(120),
            extra_headers: self.extra_headers,
            reasoning_effort: self.reasoning_effort,
            reasoning_effort_passthrough: self.reasoning_effort_passthrough,
            replay_assistant_reasoning: self.replay_assistant_reasoning_override.unwrap_or(true),
            cache_passthrough: self.cache_passthrough,
            cache_ttl: self.cache_ttl,
            thinking_passthrough: self.thinking_passthrough,
            api_path: self.api_path,
            max_tokens: self.max_tokens,
            models_dev_key: self.models_dev_key,
            openrouter_vendor_prefix: self.openrouter_vendor_prefix,
            local_model_tool_sanitize: self.local_model_tool_sanitize,
            public_model_listing: self.public_model_listing,
            tls_ca_cert_pem,
            extra_body: self.extra_body,
            schema_cache: std::sync::Arc::new(zeroclaw_api::schema::SchemaCleanCache::new()),
        }
    }
}

impl OpenAiCompatibleModelProvider {
    /// Entry point for constructing an OpenAI-compatible provider.
    ///
    /// Only `alias` is taken as a positional argument; every other field
    /// is set via labelled chain methods on the returned
    /// [`OpenAiCompatibleBuilder`] so call sites remain readable. See
    /// [`OpenAiCompatibleBuilder::build`] for the fields that must be
    /// set before calling `build()`.
    pub fn builder(alias: &str) -> OpenAiCompatibleBuilder {
        OpenAiCompatibleBuilder {
            alias: alias.to_string(),
            name: None,
            base_url: None,
            canonical_base_url: None,
            credential: None,
            auth_style: None,
            supports_vision: false,
            tool_result_image_policy: ToolResultImagePolicy::default(),
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            user_agent: None,
            merge_system_into_user: false,
            merge_system_into_user_preserve_native: false,
            native_tool_calling_override: None,
            timeout_secs: None,
            extra_headers: std::collections::HashMap::new(),
            reasoning_effort: None,
            reasoning_effort_passthrough: false,
            replay_assistant_reasoning_override: None,
            cache_passthrough: false,
            cache_ttl: None,
            thinking_passthrough: false,
            api_path: None,
            max_tokens: None,
            models_dev_key: None,
            openrouter_vendor_prefix: None,
            local_model_tool_sanitize: false,
            public_model_listing: false,
            tls_ca_cert_path: None,
            extra_body: None,
            auth_model_provider: None,
            auth_service: None,
            auth_profile_override: None,
        }
    }
    /// Add the configured custom CA certificate to a reqwest builder.
    /// The PEM bytes were loaded at construction, so this performs no disk I/O.
    fn add_tls_cert_to_builder(&self, builder: ClientBuilder) -> ClientBuilder {
        if let Some(ref pem) = self.tls_ca_cert_pem {
            match reqwest::Certificate::from_pem(pem) {
                Ok(cert) => return builder.add_root_certificate(cert),
                Err(e) => ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "Failed to parse CA certificate — TLS will use system roots"
                ),
            }
        }
        builder
    }

    /// Collect all `system` role messages and keep them in a provider-safe
    /// shape. Strict OpenAI-compatible endpoints accept a leading system
    /// message but reject system messages later in the history.
    /// Flatten system messages per `merge`. Returns the flattened list plus
    /// whether the system content was merged INTO a user message (or a
    /// synthetic user inserted for it): that first user message is the
    /// system-equivalent carrier and the only place a system-role
    /// breakpoint can ride when merging is on. `false` means a system-role
    /// message still exists on the wire (or there never was one).
    fn flatten_system_messages(messages: &[ChatMessage], merge: bool) -> (Vec<ChatMessage>, bool) {
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
            return (messages.to_vec(), false);
        }

        if system_content.is_empty() {
            return (result, false);
        }

        if !merge {
            result.insert(0, ChatMessage::system(system_content));
            return (result, false);
        }

        if let Some(first_user) = result.iter_mut().find(|m| m.role == "user") {
            first_user.content = format!("{system_content}\n\n{}", first_user.content);
        } else {
            // No user message found: insert a synthetic user message with system content
            result.insert(0, ChatMessage::user(&system_content));
        }

        (result, true)
    }

    fn http_client(&self) -> Client {
        let timeout = self.timeout_secs;
        let has_user_agent = self.user_agent.is_some();
        let has_extra_headers = !self.extra_headers.is_empty();
        let has_tls_cert = self.tls_ca_cert_pem.is_some();
        // An OpenCode client needs its own redirect policy, which the shared
        // cached client below does not carry.
        let endpoint = self.chat_completions_url();
        let targets_opencode = crate::opencode_session::is_opencode_target(&endpoint);

        if has_user_agent || has_extra_headers || has_tls_cert || targets_opencode {
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
            let builder = crate::opencode_session::restrict_redirects(builder, &endpoint);
            let builder = self.add_tls_cert_to_builder(builder);
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
                    "Failed to build proxied timeout client with custom headers or TLS certificate: "
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

    /// HTTP client for streaming SSE connections — no overall timeout (reqwest's
    /// total timeout kills long-running streams mid-response), but a `read_timeout`
    /// idle bound so a silent connection fails fast instead of hanging forever.
    /// The bound is derived as `max(STREAM_IDLE_TIMEOUT, timeout_secs)`: the 300 s
    /// floor applies when `timeout_secs` is unset or lower, and a higher
    /// `timeout_secs` raises the bound to match. Streaming paths must use this
    /// client instead of http_client().
    fn streaming_http_client(&self) -> Client {
        let endpoint = self.chat_completions_url();
        let has_user_agent = self.user_agent.is_some();
        let has_extra_headers = !self.extra_headers.is_empty();
        let has_tls_cert = self.tls_ca_cert_pem.is_some();

        if has_user_agent || has_extra_headers || has_tls_cert {
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
                .read_timeout(super::stream_idle_timeout(self.timeout_secs).duration())
                .default_headers(headers);
            let builder = crate::opencode_session::restrict_redirects(builder, &endpoint);
            let builder = self.add_tls_cert_to_builder(builder);
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
                    "Failed to build proxied streaming client with custom headers or TLS certificate: "
                );
                Client::new()
            });
        }

        let builder = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .read_timeout(super::stream_idle_timeout(self.timeout_secs).duration());
        let builder = crate::opencode_session::restrict_redirects(builder, &endpoint);
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

    fn models_url(&self) -> String {
        format!("{}/models", self.base_url)
    }

    /// Inject Anthropic prompt-cache breakpoints behind `cache_passthrough`.
    ///
    /// The native Anthropic provider is the reference for the breakpoint
    /// gate: `AnthropicModelProvider::should_cache_conversation` and
    /// `apply_cache_to_last_message` in `anthropic.rs`. (a) The system prompt
    /// always carries a breakpoint when one exists on the wire. With
    /// `merge_system_into_user`, the system role disappears from the wire,
    /// so the carrier index of the merged system content (the first user
    /// message, or the synthetic user carrying it) takes over that role and
    /// is marked unconditionally. (b) Once the conversation has more than
    /// one non-system message, a rolling breakpoint lands on the last
    /// non-system message with a non-empty text part: the last message when
    /// it carries text, otherwise the nearest earlier non-system message
    /// that does, so an image-only turn rolls the breakpoint back instead
    /// of silently dropping it. System messages are never marked twice, and
    /// when nothing qualifies there is no rolling breakpoint. At most two
    /// breakpoints per request; only breakpoint-carrying messages convert
    /// from string content to block form, every other message serializes
    /// exactly as before.
    fn apply_cache_breakpoints<T: CacheBreakpointMessage>(
        &self,
        messages: &mut [T],
        merged_system_carrier: Option<usize>,
    ) {
        if !self.cache_passthrough {
            return;
        }
        // One TTL per request: every breakpoint this pass places carries
        // the same configured lifetime. `None` resolves to the 5-minute
        // API default, whose markers serialize without a `ttl` field.
        let cache_ttl = self.cache_ttl.unwrap_or_default();
        let carrier = merged_system_carrier.and_then(|idx| {
            (idx < messages.len() && messages[idx].cache_role() != "system").then_some(idx)
        });
        match carrier {
            Some(idx) => {
                if let Some(content) = messages[idx].cache_content() {
                    content.apply_cache_control(cache_ttl);
                }
            }
            None => {
                if let Some(system) = messages.iter_mut().find(|m| m.cache_role() == "system")
                    && let Some(content) = system.cache_content()
                {
                    content.apply_cache_control(cache_ttl);
                }
            }
        }
        let non_system_count = messages
            .iter()
            .filter(|m| m.cache_role() != "system")
            .count();
        if non_system_count > 1 {
            for message in messages.iter_mut().rev() {
                if message.cache_role() == "system" {
                    continue;
                }
                if let Some(content) = message.cache_content()
                    && content.apply_cache_control(cache_ttl)
                {
                    break;
                }
            }
        }
    }

    /// Index of the merged system-content carrier (the first user message)
    /// when `flatten_system_messages` reported a merge, else `None`.
    fn merged_system_carrier_index<T: CacheBreakpointMessage>(
        messages: &[T],
        system_merged: bool,
    ) -> Option<usize> {
        if system_merged {
            messages.iter().position(|m| m.cache_role() == "user")
        } else {
            None
        }
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
        // The name filter below exists because some OpenAI-compatible
        // backends reject unknown request params (HTTP 400 for
        // `reasoning_effort` on models they do not treat as reasoners).
        // Operators who have verified their backend honors the param —
        // GLM/Kimi/DeepSeek/Qwen-style reasoners behind OpenAI-compatible
        // gateways commonly do — can bypass the filter per provider.
        if self.reasoning_effort_passthrough {
            return Some(effort.clone());
        }
        let id = model
            .rsplit('/')
            .next()
            .unwrap_or(model)
            .to_ascii_lowercase();
        // gpt-5*-chat-latest (gpt-5-chat-latest, gpt-5.1-chat-latest, ...) are
        // OpenAI's non-reasoning chat-router models; the Chat Completions API
        // rejects reasoning_effort for them. Treat them as a distinct family, the
        // same way the native openai.rs provider already special-cases them.
        let is_gpt5_chat_latest = id.starts_with("gpt-5") && id.ends_with("-chat-latest");
        let is_openai_reasoning_model = id == "o1"
            || id.starts_with("o1-")
            || id == "o3"
            || id.starts_with("o3-")
            || id == "o4"
            || id.starts_with("o4-")
            || (id.starts_with("gpt-5") && !is_gpt5_chat_latest);
        let is_likely_codex_supported = id.contains("codex") && id.starts_with("gpt-");

        (is_openai_reasoning_model || is_likely_codex_supported).then(|| effort.clone())
    }

    async fn resolve_credential(&self) -> anyhow::Result<Option<String>> {
        if self
            .credential
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
        {
            return Ok(self.credential.clone());
        }
        let (Some(auth), Some(model_provider)) = (&self.auth_service, &self.auth_model_provider)
        else {
            return Ok(None);
        };
        if model_provider == "xai" {
            return auth
                .get_valid_xai_access_token(self.auth_profile_override.as_deref())
                .await;
        }
        auth.get_provider_bearer_token(model_provider, self.auth_profile_override.as_deref())
            .await
    }

    fn assistant_reasoning_value(value: &serde_json::Value) -> Option<&str> {
        value
            .get("reasoning_content")
            .and_then(serde_json::Value::as_str)
            .or_else(|| value.get("reasoning").and_then(serde_json::Value::as_str))
    }

    /// Replay payload for an outbound assistant history message.
    ///
    /// Flag off: today's behavior — the stored reasoning strings replay
    /// verbatim. Flag on: history reasoning is expected as newline-delimited
    /// signed-JSON envelope lines (the capture format); signed lines
    /// reconstruct a `thinking_blocks` array and suppress the string fields.
    /// Unsigned or malformed history sends nothing — blocks are never
    /// fabricated, and a bare reasoning string is exactly what translating
    /// gateways reject. `replay_assistant_reasoning = false` still wins:
    /// nothing at all.
    fn assistant_thinking_replay(&self, value: &serde_json::Value) -> AssistantThinkingReplay {
        if !self.replay_assistant_reasoning {
            return AssistantThinkingReplay::none();
        }
        if !self.thinking_passthrough {
            let (reasoning_content, reasoning) = Self::assistant_reasoning_pair(value);
            return AssistantThinkingReplay {
                reasoning_content,
                reasoning,
                thinking_blocks: None,
            };
        }
        let Some(reasoning) = Self::assistant_reasoning_value(value) else {
            return AssistantThinkingReplay::none();
        };
        match Self::reasoning_lines_to_thinking_blocks(reasoning) {
            Some(blocks) => AssistantThinkingReplay {
                reasoning_content: None,
                reasoning: None,
                thinking_blocks: Some(blocks),
            },
            // Unsigned/malformed: documented no-op.
            None => AssistantThinkingReplay::none(),
        }
    }

    /// Field-level reasoning pair, ignoring the replay gate. Shared by the
    /// flag-off branch of [`Self::assistant_thinking_replay`].
    fn assistant_reasoning_pair(value: &serde_json::Value) -> (Option<String>, Option<String>) {
        let reasoning_content = value
            .get("reasoning_content")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string);
        let reasoning = value
            .get("reasoning")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string);
        (reasoning_content, reasoning)
    }

    /// Signed-block replay for the history fallback builder. Mirrors the
    /// native request converter's assistant handling: when the stored
    /// content is the runtime's reasoning envelope (a JSON object carrying
    /// `reasoning_content`), run the same validated reconstruction and
    /// attach the resulting `thinking_blocks`. Gated by the same provider
    /// flags as the converter, so flag-off history produces no field.
    fn fallback_thinking_replay(&self, message: &ChatMessage) -> Option<Vec<serde_json::Value>> {
        if !self.thinking_passthrough || !self.replay_assistant_reasoning {
            return None;
        }
        if message.role != "assistant" {
            return None;
        }
        let value = serde_json::from_str::<serde_json::Value>(&message.content).ok()?;
        self.assistant_thinking_replay(&value).thinking_blocks
    }

    /// Reconstruct `thinking_blocks` from newline-delimited signed-JSON
    /// envelope lines. Replay whitelist mirrors capture: signed `thinking`
    /// envelopes and well-formed `redacted_thinking` lines (opaque `data`
    /// replayed verbatim, signature-less by design) are forwarded; well-formed
    /// lines of any other type are skipped, not forwarded. Lines that fail to
    /// parse and thinking lines without a signature void the whole replay:
    /// Anthropic rejects unsigned thinking blocks on input, and the caller
    /// sends nothing rather than fabricating or forwarding a partial
    /// sequence. Mirrors the native Anthropic provider's replay parsing
    /// (`crates/zeroclaw-providers/src/anthropic.rs`) — duplicate-with-comment
    /// per the passthrough spec boundary.
    fn reasoning_lines_to_thinking_blocks(reasoning: &str) -> Option<Vec<serde_json::Value>> {
        let mut blocks = Vec::new();
        for line in reasoning.split('\n') {
            if line.trim().is_empty() {
                continue;
            }
            let parsed: serde_json::Value = serde_json::from_str(line).ok()?;
            let block_type = parsed
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("thinking");
            if block_type == "redacted_thinking" {
                let has_data = parsed
                    .get("data")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|data| !data.is_empty());
                if has_data {
                    blocks.push(parsed);
                }
                continue;
            }
            if block_type != "thinking" {
                continue;
            }
            let thinking = parsed.get("thinking").and_then(serde_json::Value::as_str)?;
            let signature = parsed
                .get("signature")
                .and_then(serde_json::Value::as_str)
                .filter(|signature| !signature.is_empty())?;
            blocks.push(serde_json::json!({
                "type": "thinking",
                "thinking": thinking,
                "signature": signature,
            }));
        }
        (!blocks.is_empty()).then_some(blocks)
    }

    /// Anthropic-shaped `thinking` request object for the given model and
    /// runtime params, when [`Self::thinking_passthrough`] is on and the
    /// runtime supplied native thinking params.
    ///
    /// Style resolution is shared with the native Anthropic provider via
    /// `crate::anthropic::anthropic_thinking_style` (same crate, free
    /// function): budget models get the fixed-budget `enabled` shape,
    /// adaptive-only models (Opus 4.7, the whole Fable 5 family) get
    /// `{"type":"adaptive"}` with no `budget_tokens`, matching the native
    /// `NativeThinkingConfig` serialization. `params.display` rides on both
    /// shapes as the snake_case `display` key when present, exactly as the
    /// native provider emits it. Gateway model IDs may carry routing
    /// prefixes; the resolver's substring matching handles them.
    /// History-path chat with optional thinking rethreading. Fallback paths
    /// that rebuild a tool request as prompt-guided text pass the original
    /// `request.thinking` so the rebuilt body keeps the same injection the
    /// primary path would have sent. Returns the parsed gateway message
    /// plus its usage; projecting that into legacy text is the caller's
    /// choice (`legacy_history_text` for the `String`-returning path).
    async fn chat_with_history_inner(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
        thinking: Option<zeroclaw_api::model_provider::NativeThinkingParams>,
    ) -> anyhow::Result<(
        ResponseMessage,
        Option<zeroclaw_api::model_provider::TokenUsage>,
    )> {
        let credential = self.resolve_credential().await?;

        let normalized = self.normalize_messages_for_upstream(messages).await?;
        let merge = self.effective_merge_system(model);
        let (effective_messages, _system_merged) =
            Self::flatten_system_messages(&normalized, merge);
        // Strip native tool constructs for non-native-tool model_providers.
        let effective_messages = self.strip_native_tool_messages(&effective_messages);
        let api_messages: Vec<Message> = effective_messages
            .iter()
            .map(|m| Message {
                role: m.role.clone(),
                content: self.message_content_for_role(&m.role, &m.content, !merge, false),
                thinking_blocks: self.fallback_thinking_replay(m),
            })
            .collect();

        let shape = self.resolve_request_shape(model, thinking, temperature, self.max_tokens);
        let request = ApiChatRequest {
            model: model.to_string(),
            messages: api_messages,
            temperature: shape.temperature,
            stream: Some(false),
            stream_options: None,
            reasoning_effort: self.reasoning_effort_for_model(model),
            tool_stream: None,
            tools: None,
            tool_choice: None,
            max_tokens: shape.max_tokens,
            extra_body: shape.extra_body,
        };
        // No cache breakpoints here, deliberately: the `String`-returning
        // `chat_with_history` wrapper drops response usage, so a premium
        // cache write it triggered could never be accounted for. The
        // structured schema-fallback re-entry in `chat` does surface usage,
        // but the rule still holds: this shared request builder exists for
        // the text-only contract, whose wrapper cannot account a cache
        // write. The cache flag is honored on the structured paths (`chat`,
        // `chat_with_tools`, `stream_chat`), which capture usage; the
        // provider docs describe this usage-capture boundary.

        let url = self.chat_completions_url();
        let response = match self
            .apply_opencode_session_header(self.apply_auth_header(
                self.http_client().post(&url).json(&request),
                credential.as_deref(),
            )?)
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
        let usage = chat_response.usage.map(UsageInfo::into_provider_usage);

        chat_response
            .choices
            .into_iter()
            .next()
            .map(|choice| (choice.message, usage))
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

    fn thinking_request_object(
        &self,
        model: &str,
        thinking: Option<zeroclaw_api::model_provider::NativeThinkingParams>,
    ) -> Option<serde_json::Value> {
        if !self.thinking_passthrough {
            return None;
        }
        let params = thinking?;
        let mut object = match crate::anthropic::anthropic_thinking_style(model) {
            crate::anthropic::AnthropicThinkingStyle::Adaptive => {
                serde_json::json!({"type": "adaptive"})
            }
            crate::anthropic::AnthropicThinkingStyle::Budget => serde_json::json!({
                "type": "enabled",
                "budget_tokens": params.budget_tokens
            }),
        };
        if let Some(display) = params.display {
            object["display"] = serde_json::json!(display.as_str());
        }
        Some(serde_json::json!({ "thinking": object }))
    }

    /// Effective `extra_body` for a request body: the injected `thinking`
    /// object (when any) merged under the configured `extra_body`, whose
    /// explicit keys always win. With the flag off or no params supplied,
    /// this is exactly the configured `extra_body` (byte-identical default).
    fn request_extra_body(
        &self,
        model: &str,
        thinking: Option<zeroclaw_api::model_provider::NativeThinkingParams>,
    ) -> Option<serde_json::Value> {
        let Some(mut merged) = self.thinking_request_object(model, thinking) else {
            return self.extra_body.clone();
        };
        match self.extra_body.as_ref() {
            None => Some(merged),
            Some(extra) if extra.is_object() => {
                if let (Some(base), Some(over)) = (merged.as_object_mut(), extra.as_object()) {
                    base.extend(over.clone());
                }
                Some(merged)
            }
            // Non-object `extra_body` cannot be key-merged; the explicit
            // config value wins outright (preserving its existing behavior).
            Some(extra) => Some(extra.clone()),
        }
    }

    /// Effective request shape for a body that may carry thinking
    /// passthrough: the merged `extra_body` plus the normalized temperature
    /// and output limit, resolved together so the three fields can never
    /// disagree about which `thinking` object is being sent. Normalization
    /// follows the `thinking` object that will actually be serialized —
    /// the injected one, or the operator's explicit
    /// `provider_extra.thinking` override when that key wins the merge —
    /// so an `enabled` or `adaptive` effective object forces temperature
    /// 1.0 (Anthropic rejects extended thinking combined with a modified
    /// temperature) and an `enabled` object with an integer
    /// `budget_tokens` raises `max_tokens` above that effective budget
    /// (the API requires the limit to strictly exceed the budget), while
    /// any other effective shape keeps the caller's values. With the flag
    /// off or no params supplied nothing is injected and nothing is
    /// normalized, keeping flag-off requests byte-identical; an operator's
    /// configured `extra_body` carrying its own `thinking` key with the
    /// flag off is that explicit request and is left alone.
    fn resolve_request_shape(
        &self,
        model: &str,
        thinking: Option<zeroclaw_api::model_provider::NativeThinkingParams>,
        temperature: Option<f64>,
        max_tokens: Option<u32>,
    ) -> EffectiveRequestShape {
        let extra_body = self.request_extra_body(model, thinking);
        if self.thinking_request_object(model, thinking).is_none() {
            return EffectiveRequestShape {
                extra_body,
                temperature,
                max_tokens,
            };
        }
        // The `thinking` object that will actually be serialized: the
        // injected one, or the operator's explicit override when the
        // configured `extra_body` key wins the merge.
        let effective_thinking = extra_body.as_ref().and_then(|body| body.get("thinking"));
        let thinking_type = effective_thinking
            .and_then(|object| object.get("type"))
            .and_then(serde_json::Value::as_str);
        if !matches!(thinking_type, Some("enabled") | Some("adaptive")) {
            // `off` or any other explicit shape (including a non-object
            // `extra_body` that replaced the merge): the operator pinned a
            // non-thinking request, so their temperature and limit stand.
            return EffectiveRequestShape {
                extra_body,
                temperature,
                max_tokens,
            };
        }
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"model": model})),
            "Passthrough extended thinking enabled; forcing temperature=1.0"
        );
        if thinking_type == Some("adaptive") {
            // Adaptive thinking carries no budget; the configured limit
            // stands.
            return EffectiveRequestShape {
                extra_body,
                temperature: Some(1.0),
                max_tokens,
            };
        }
        // The API requires max_tokens > budget_tokens (strictly greater),
        // measured against the budget actually being sent.
        let min_required = effective_thinking
            .and_then(|object| object.get("budget_tokens"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|budget| u32::try_from(budget).ok())
            .and_then(|budget| budget.checked_add(1));
        let max_tokens = match min_required {
            Some(min_required) => {
                let raised = max_tokens.max(Some(min_required));
                if raised != max_tokens {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"model": model})),
                        "Passthrough thinking budget meets or exceeds configured max_tokens; raising output limit"
                    );
                }
                raised
            }
            None => {
                // The effective object carries no integer budget (an
                // override asked for a shape whose limit relation cannot
                // be validated here); leave the configured limit alone and
                // let the gateway report an invalid shape.
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"model": model})),
                    "Passthrough thinking object carries no integer budget_tokens; leaving max_tokens unchanged"
                );
                max_tokens
            }
        };
        EffectiveRequestShape {
            extra_body,
            temperature: Some(1.0),
            max_tokens,
        }
    }
}

/// Resolved request-shape fields for a body that may carry thinking
/// passthrough: the merged `extra_body` plus the normalized temperature
/// and output limit, produced together by
/// [`OpenAiCompatibleModelProvider::resolve_request_shape`] so the three
/// fields cannot disagree about which `thinking` object is being sent.
#[derive(Debug)]
struct EffectiveRequestShape {
    extra_body: Option<serde_json::Value>,
    temperature: Option<f64>,
    max_tokens: Option<u32>,
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
    /// Extra fields merged at the top level of the serialized JSON body.
    /// Mirrors `NativeChatRequest::extra_body` so config-driven extras
    /// (`provider_extra`, `chat_template_kwargs`) reach the no-tools request
    /// paths too, not just the native-tools path.
    #[serde(flatten)]
    extra_body: Option<serde_json::Value>,
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
    /// Anthropic `thinking_blocks` replay on assistant turns, populated only
    /// when the passthrough flag is on and the stored content is a runtime
    /// reasoning envelope with validated signed blocks: the same
    /// reconstruction the native request converter performs. Absent
    /// otherwise, keeping flag-off fallback requests byte-identical. This is
    /// what keeps the second schema-fallback turn's signed trajectory
    /// intact: the two-field fallback builder must replay what the primary
    /// converter would have replayed.
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_blocks: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Parts(Vec<MessagePart>),
}

impl MessageContent {
    /// Mark this content as an Anthropic prompt-cache breakpoint and return
    /// whether a markable text part was found: plain string content
    /// converts to the single-text-block wire form (block conversion
    /// touches only breakpoint-carrying messages); block-form content gains
    /// the marker on its last non-empty text part. A message ending in an
    /// image part therefore carries the breakpoint on its text, and the
    /// image part stays unmarked: the image is covered by the following
    /// turn's rolling breakpoint. Empty text is never marked, because the
    /// wire format rejects empty text blocks that carry `cache_control`.
    fn apply_cache_control(&mut self, cache_ttl: CacheTtl) -> bool {
        match self {
            MessageContent::Text(text) => {
                if text.is_empty() {
                    return false;
                }
                *self = MessageContent::Parts(vec![MessagePart::Text {
                    text: std::mem::take(text),
                    cache_control: Some(crate::anthropic::CacheControl::ephemeral_with_ttl(
                        cache_ttl,
                    )),
                }]);
                true
            }
            MessageContent::Parts(parts) => {
                for part in parts.iter_mut().rev() {
                    if let MessagePart::Text {
                        text,
                        cache_control,
                    } = part
                        && !text.is_empty()
                    {
                        *cache_control = Some(crate::anthropic::CacheControl::ephemeral_with_ttl(
                            cache_ttl,
                        ));
                        return true;
                    }
                }
                false
            }
        }
    }
}

/// Uniform (role, content) access over the two wire message shapes so one
/// breakpoint-injection routine serves both the no-tools and the native-tools
/// request types.
trait CacheBreakpointMessage {
    fn cache_role(&self) -> &str;
    fn cache_content(&mut self) -> Option<&mut MessageContent>;
}

impl CacheBreakpointMessage for Message {
    fn cache_role(&self) -> &str {
        &self.role
    }

    fn cache_content(&mut self) -> Option<&mut MessageContent> {
        Some(&mut self.content)
    }
}

impl CacheBreakpointMessage for NativeMessage {
    fn cache_role(&self) -> &str {
        &self.role
    }

    fn cache_content(&mut self) -> Option<&mut MessageContent> {
        self.content.as_mut()
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum MessagePart {
    Text {
        text: String,
        /// Anthropic prompt-cache breakpoint, injected only behind
        /// `cache_passthrough` and only on breakpoint-carrying messages.
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<crate::anthropic::CacheControl>,
    },
    ImageUrl {
        image_url: ImageUrlPart,
    },
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

/// OpenAI-compatible chat response, either in the standard top-level shape or
/// wrapped by a gateway in a top-level `data` object.
///
/// Keep the direct variant first: a valid top-level response remains
/// authoritative when a provider also includes a `data` metadata field.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ApiChatResponseEnvelope {
    Direct(ApiChatResponse),
    Wrapped { data: ApiChatResponse },
}

impl ApiChatResponseEnvelope {
    fn into_response(self) -> ApiChatResponse {
        match self {
            Self::Direct(response) => response,
            Self::Wrapped { data } => data,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
struct UsageInfo {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
    #[serde(default, deserialize_with = "deserialize_optional_token_count")]
    prompt_cache_hit_tokens: Option<u64>,
    /// Anthropic-shaped cache counters, forwarded by translating gateways
    /// on Anthropic-backed routes. `cache_read_input_tokens` is the
    /// authoritative cached-read count (it comes from the upstream API
    /// itself), so it takes precedence over the gateway-accounted OpenAI
    /// and DeepSeek shapes.
    #[serde(default, deserialize_with = "deserialize_optional_token_count")]
    cache_read_input_tokens: Option<u64>,
    /// Anthropic-shaped cache-write counter. Not part of `TokenUsage`
    /// (cached reads are the billing-relevant figure there); logged so
    /// write-premium spend is visible in logs.
    #[serde(default, deserialize_with = "deserialize_optional_token_count")]
    cache_creation_input_tokens: Option<u64>,
}

#[derive(Debug, Deserialize, Clone)]
struct PromptTokensDetails {
    #[serde(default, deserialize_with = "deserialize_optional_token_count")]
    cached_tokens: Option<u64>,
    /// Cache-write tokens reported by translating gateways that forward
    /// Anthropic usage (`prompt_tokens_details.cache_creation_input_tokens`).
    #[serde(default, deserialize_with = "deserialize_optional_token_count")]
    cache_creation_input_tokens: Option<u64>,
}

impl UsageInfo {
    /// Cached-read tokens across the three coexisting provider shapes.
    /// Precedence: Anthropic `cache_read_input_tokens` (upstream-reported
    /// on translated routes) over DeepSeek `prompt_cache_hit_tokens` over
    /// OpenAI `prompt_tokens_details.cached_tokens` (gateway-accounted).
    fn cached_input_tokens(&self) -> Option<u64> {
        self.cache_read_input_tokens
            .or(self.prompt_cache_hit_tokens)
            .or_else(|| {
                self.prompt_tokens_details
                    .as_ref()
                    .and_then(|details| details.cached_tokens)
            })
    }

    fn cache_creation_input_tokens(&self) -> Option<u64> {
        self.prompt_tokens_details
            .as_ref()
            .and_then(|details| details.cache_creation_input_tokens)
    }

    fn into_provider_usage(self) -> zeroclaw_api::model_provider::TokenUsage {
        if let Some(creation) = self.cache_creation_input_tokens.filter(|count| *count > 0) {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success)
                    .with_attrs(::serde_json::json!({
                        "cache_creation_input_tokens": creation,
                        "cache_read_input_tokens": self.cache_read_input_tokens,
                    })),
                "gateway-reported Anthropic cache write (billed at the write premium)"
            );
        }
        let cached_input_tokens = self.cached_input_tokens();
        let cache_creation_input_tokens = self.cache_creation_input_tokens();
        zeroclaw_api::model_provider::TokenUsage {
            input_tokens: self.prompt_tokens,
            output_tokens: self.completion_tokens,
            cached_input_tokens,
            cache_creation_input_tokens,
        }
    }
}

fn deserialize_optional_token_count<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(normalize_token_count_value))
}

fn normalize_token_count_value(value: serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::Number(number) => {
            if let Some(value) = number.as_u64() {
                Some(value)
            } else if let Some(value) = number.as_i64() {
                if value < 0 {
                    None
                } else {
                    u64::try_from(value).ok()
                }
            } else {
                number.as_f64().and_then(normalize_token_count_float)
            }
        }
        serde_json::Value::String(value) => value
            .trim()
            .parse::<f64>()
            .ok()
            .and_then(normalize_token_count_float),
        _ => None,
    }
}

fn normalize_token_count_float(value: f64) -> Option<u64> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    if value < 1.0 {
        return Some(0);
    }
    if value > u64::MAX as f64 {
        return None;
    }
    Some(value.floor() as u64)
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
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
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<ToolCall>>,
    /// Raw gateway `thinking_blocks` array, preserved so
    /// [`OpenAiCompatibleModelProvider::parse_native_response`] can
    /// normalize it behind the `thinking_passthrough` flag. Never
    /// serialized.
    #[serde(skip_serializing)]
    thinking_blocks: Option<Vec<serde_json::Value>>,
}

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
    /// Anthropic thinking blocks forwarded by translating gateways
    /// (e.g. `thinking_blocks` on LiteLLM / passthrough responses).
    #[serde(default)]
    thinking_blocks: Option<Vec<serde_json::Value>>,
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
            thinking_blocks: raw.thinking_blocks,
        }
    }
}

impl ResponseMessage {
    /// Extract text content from the `content` field only. Does NOT fall
    /// back to `reasoning_content` — thinking/reasoning models (GLM-5.1,
    /// DeepSeek, Qwen) return their thinking in `reasoning_content` which
    /// must not leak into the user-visible response text. The
    /// `reasoning_content` is preserved separately in
    /// `ChatResponse.reasoning_content` for history round-tripping.
    ///
    /// Returns the `content` field as-is. Previously this stripped
    /// `<think>...</think>` blocks that some reasoning models (e.g. MiniMax)
    /// embedded inline in `content` instead of using a separate field, but
    /// that unconditional rewrite silently mangled responses whose `content`
    /// legitimately contained literal `<think>...</think>` markup (HTML, code
    /// samples, quoted discussion of the tag itself, and unclosed tails).
    /// Model providers that need inline think-block filtering should do it
    /// downstream of this response shape, with full visibility into the
    /// model's actual output.
    fn effective_content(&self) -> String {
        self.content
            .as_ref()
            .cloned()
            .filter(|c| !c.is_empty())
            .unwrap_or_default()
    }

    fn effective_content_optional(&self) -> Option<String> {
        self.content.as_ref().cloned().filter(|c| !c.is_empty())
    }
}

/// Text-only history contract, carried over from the pre-structured-path
/// behavior: callers that parse tool calls back out of text depend on the
/// JSON shape, so a message with non-empty `tool_calls` serializes the
/// whole gateway message; every other shape reduces to visible content.
fn legacy_history_text(message: &ResponseMessage) -> String {
    if message
        .tool_calls
        .as_ref()
        .is_some_and(|t: &Vec<_>| !t.is_empty())
    {
        serde_json::to_string(message).unwrap_or_else(|_| message.effective_content())
    } else {
        message.effective_content()
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
struct NativeChatRequest<T = Vec<NativeToolSpec>> {
    model: String,
    messages: Vec<NativeMessage>,
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
    tools: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    /// Extra fields merged at the top level of the serialized JSON body.
    #[serde(flatten)]
    extra_body: Option<serde_json::Value>,
}

/// Ensure the serialized request body carries an explicit
/// `reasoning_effort: "none"`, reporting whether that changed the payload.
///
/// `extra_body` is flattened into [`NativeChatRequest`] and can supply the
/// canonical top-level value seen by the endpoint, so fallback decisions must
/// inspect and mutate the wire payload rather than only the typed field.
///
/// An *absent* `reasoning_effort` is not equivalent to `"none"`: endpoints
/// default an omitted effort to a non-`none` value, so a payload without the
/// field is rejected exactly like one carrying `"high"`. The key is therefore
/// inserted when missing.
///
/// Returns `false` once the payload already states exactly `"none"`, which is
/// the fixed point that bounds the retry to a single additional request. The
/// comparison is case-sensitive on purpose: a differently-spelled `"NONE"`
/// supplied through `provider_extra` can be rejected by a case-sensitive
/// endpoint, so it is normalized to the canonical lowercase value rather than
/// treated as already repaired. Rewriting a non-canonical spelling still
/// converges after one retry because the rewritten payload is the fixed point.
fn ensure_reasoning_effort_none(payload: &mut serde_json::Value) -> bool {
    let Some(object) = payload.as_object_mut() else {
        return false;
    };
    if matches!(
        object.get("reasoning_effort"),
        Some(serde_json::Value::String(effort)) if effort == "none"
    ) {
        return false;
    }
    object.insert(
        "reasoning_effort".to_string(),
        serde_json::Value::String("none".to_string()),
    );
    true
}

/// Replay fields for an outbound assistant history message — see
/// [`OpenAiCompatibleModelProvider::assistant_thinking_replay`].
struct AssistantThinkingReplay {
    reasoning_content: Option<String>,
    reasoning: Option<String>,
    thinking_blocks: Option<Vec<serde_json::Value>>,
}

impl AssistantThinkingReplay {
    fn none() -> Self {
        Self {
            reasoning_content: None,
            reasoning: None,
            thinking_blocks: None,
        }
    }
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
    #[serde(skip_serializing_if = "Option::is_none", rename = "reasoning")]
    reasoning: Option<String>,
    /// Reconstructed Anthropic thinking blocks for assistant history replay
    /// under `thinking_passthrough` (signed envelope lines only). Never
    /// populated alongside the string reasoning fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_blocks: Option<Vec<serde_json::Value>>,
    /// Tool name for `role: "tool"` messages. Groq native tool calling
    /// requires this field on every tool-result message; omitting it causes
    /// HTTP 400 "Tools should have a name!"./
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
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
    reasoning_content: Option<String>,
    /// Native tool-calling deltas in OpenAI chat-completions streaming format.
    tool_calls: Option<Vec<StreamToolCallDelta>>,
}

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
        // Route through the shared `sanitize_tool_arguments` helper so the
        // normalization contract (empty/whitespace → "{}", invalid JSON →
        // WARN + "{}", valid JSON → passthrough) has a single source of truth.
        let normalized_arguments = sanitize_tool_arguments(&name, &self.arguments);

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
/// Convert an SSE byte stream into structured chunks. `idle_timeout` is the
/// streaming client's read-idle bound; it names the bound that fired in
/// body-read timeout errors, including whether `timeout_secs` can raise it.
fn sse_bytes_to_chunks(
    response: reqwest::Response,
    count_tokens: bool,
    idle_timeout: super::StreamIdleBound,
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

        'stream: while let Some(item) = bytes_stream.next().await {
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

                        if line.trim().strip_prefix("data:").map(str::trim) == Some("[DONE]") {
                            break 'stream;
                        }

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
                        .send(Err(StreamError::Http(super::stream_idle_error_message(
                            &e,
                            idle_timeout,
                        ))))
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
    sse_bytes_to_events_for_contract(
        response,
        count_tokens,
        false,
        super::StreamIdleBound::Fixed(super::STREAM_IDLE_TIMEOUT),
    )
}

fn sse_bytes_to_events_for_contract(
    response: reqwest::Response,
    count_tokens: bool,
    targets_mistral_tool_call_contract: bool,
    idle_timeout: super::StreamIdleBound,
) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(100);

    let handle = ::zeroclaw_spawn::spawn!(async move {
        let mut buffer = String::new();
        let mut tool_calls: Vec<StreamToolCallAccumulator> = Vec::new();
        let mut used_tool_call_ids = std::collections::HashSet::new();
        let mut emitted_tool_calls = false;
        let mut saw_completion = false;

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
        'stream: while let Some(item) = bytes_stream.next().await {
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
                            Ok(None) => {
                                if line.trim().strip_prefix("data:").map(str::trim)
                                    == Some("[DONE]")
                                {
                                    saw_completion = true;
                                    break 'stream;
                                }
                                continue;
                            }
                            Err(e) => {
                                let _ = tx.send(Err(e)).await;
                                return;
                            }
                        };

                        let mut should_emit_tool_calls = false;
                        for choice in &chunk.choices {
                            if choice.finish_reason.is_some() {
                                saw_completion = true;
                            }
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

                        if let Some(usage) = chunk.usage.clone() {
                            let token_usage = usage.into_provider_usage();
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
                        .send(Err(StreamError::Http(super::stream_idle_error_message(
                            &e,
                            idle_timeout,
                        ))))
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

        crate::stream_guard::finish_sse_stream(&tx, saw_completion, "[DONE] or finish_reason")
            .await;
    });

    let guard = AbortOnDrop::new(handle.abort_handle());
    stream::unfold((rx, guard), |(mut rx, guard)| async move {
        rx.recv().await.map(|event| (event, (rx, guard)))
    })
    .boxed()
}

fn parse_chat_response_body(name: &str, body: &str) -> anyhow::Result<ApiChatResponse> {
    serde_json::from_str::<ApiChatResponseEnvelope>(body)
        .map(ApiChatResponseEnvelope::into_response)
        .map_err(|_| {
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
    /// `Err` when the stored credential cannot be turned into a header for
    /// this provider's [`AuthStyle`]. Callers propagate it rather than send:
    /// the request would carry no credential at all and be rejected upstream.
    fn apply_auth_header(
        &self,
        req: reqwest::RequestBuilder,
        credential: Option<&str>,
    ) -> anyhow::Result<reqwest::RequestBuilder> {
        apply_auth_to_request(req, &self.auth_header, credential, &self.name)
    }

    /// OpenCode affinity header value for the calling conversation, or `None`
    /// when this provider does not target OpenCode.
    ///
    /// Classifies `chat_completions_url()`, the URL every header-carrying
    /// request is sent to, rather than `base_url`: an `api_path` is appended to
    /// the base, so the base alone need not name the destination host.
    ///
    /// Returns `None` when the operator has already pinned a valid header value
    /// through `extra_headers`: those are baked into the client's default
    /// headers, so adding a second value here would put the header on the wire
    /// twice. A pinned value the client builder skips as invalid does not count.
    fn opencode_session_value(&self) -> Option<String> {
        if crate::opencode_session::operator_pinned_session(&self.extra_headers) {
            return None;
        }
        crate::opencode_session::session_token(&self.chat_completions_url())
    }

    /// Attach the OpenCode affinity header, for request paths that build in the
    /// caller's task.
    ///
    /// Streaming paths must not use this: they build inside
    /// `zeroclaw_spawn::spawn!`, where the conversation task-local is no longer
    /// readable. Those resolve `opencode_session_value` before the spawn.
    fn apply_opencode_session_header(
        &self,
        req: reqwest::RequestBuilder,
    ) -> reqwest::RequestBuilder {
        match self.opencode_session_value() {
            Some(session) => req.header(OPENCODE_SESSION_HEADER, session),
            None => req,
        }
    }

    fn convert_tool_specs(
        &self,
        tools: Option<&[zeroclaw_api::tool::ToolSpec]>,
    ) -> Option<Vec<NativeToolSpec>> {
        tools.map(|items| {
            items
                .iter()
                .map(|tool| NativeToolSpec {
                    kind: "function".to_string(),
                    extra: serde_json::Map::new(),
                    function: NativeToolFunctionSpec {
                        extra: serde_json::Map::new(),
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        // Cleaned at most once per registered schema per
                        // provider instance (memoized), then `Arc`-shared into every request
                        // body — never deep-copied per request.
                        parameters: self.schema_cache.clean_shared(
                            &tool.parameters,
                            zeroclaw_api::schema::CleaningStrategy::OpenAI,
                        ),
                    },
                })
                .collect()
        })
    }

    fn convert_tool_specs_for_model(
        &self,
        tools: Option<&[zeroclaw_api::tool::ToolSpec]>,
        model: &str,
    ) -> Option<Vec<NativeToolSpec>> {
        let mut converted = self.convert_tool_specs(tools)?;
        if !self.local_model_tool_sanitize || !Self::should_sanitize_local_tool_schema(model) {
            return Some(converted);
        }
        // Preserve the pre-existing compatible-provider wire behavior in
        // this allocation-only change. The legacy sanitizer inspected a
        // top-level `parameters` extension even though ordinary OpenAI tool
        // specs place it under `function`; activating a nested rewrite is a
        // separate protocol change that needs its own compatibility contract.
        for tool in &mut converted {
            let Some(raw_parameters) = tool.extra.get("parameters").cloned() else {
                continue;
            };
            let cleaned = zeroclaw_api::schema::SchemaCleanr::clean(
                raw_parameters,
                zeroclaw_api::schema::CleaningStrategy::Conservative,
            );
            tool.extra.insert("parameters".to_string(), cleaned);
        }
        Some(converted)
    }

    fn should_sanitize_local_tool_schema(model: &str) -> bool {
        let lower = model.to_ascii_lowercase();
        model.is_empty() || lower.contains("gemma-4") || lower.contains("gemma4")
    }

    fn build_native_tool_chat_request(
        &self,
        effective_messages: &[ChatMessage],
        tools: Option<Vec<NativeToolSpec>>,
        model: &str,
        temperature: Option<f64>,
        allow_user_image_parts: bool,
        system_merged: bool,
        thinking: Option<zeroclaw_api::model_provider::NativeThinkingParams>,
    ) -> NativeChatRequest {
        let has_tool_entries = tools.as_ref().is_some_and(|tools| !tools.is_empty());
        let tool_choice = has_tool_entries.then(|| "auto".to_string());
        let mut messages =
            self.convert_messages_for_native(effective_messages, allow_user_image_parts);
        let carrier = Self::merged_system_carrier_index(&messages, system_merged);
        self.apply_cache_breakpoints(&mut messages, carrier);

        let shape = self.resolve_request_shape(model, thinking, temperature, self.max_tokens);
        NativeChatRequest {
            model: model.to_string(),
            messages,
            temperature: shape.temperature,
            stream: Some(false),
            // Non-streaming path; `usage` is on the final response body, not
            // gated on `stream_options.include_usage`.
            stream_options: None,
            reasoning_effort: self.reasoning_effort_for_model(model),
            tool_stream: self.tool_stream_for_tools(has_tool_entries),
            tools,
            tool_choice,
            max_tokens: shape.max_tokens,
            extra_body: shape.extra_body,
        }
    }

    fn build_raw_native_tool_chat_request<'a>(
        &self,
        effective_messages: &[ChatMessage],
        tools: Option<&'a [serde_json::Value]>,
        model: &str,
        temperature: Option<f64>,
        allow_user_image_parts: bool,
        system_merged: bool,
        thinking: Option<zeroclaw_api::model_provider::NativeThinkingParams>,
    ) -> NativeChatRequest<&'a [serde_json::Value]> {
        let has_tool_entries = tools.is_some_and(|tools| !tools.is_empty());
        let mut messages =
            self.convert_messages_for_native(effective_messages, allow_user_image_parts);
        let carrier = Self::merged_system_carrier_index(&messages, system_merged);
        self.apply_cache_breakpoints(&mut messages, carrier);
        let shape = self.resolve_request_shape(model, thinking, temperature, self.max_tokens);
        NativeChatRequest {
            model: model.to_string(),
            messages,
            temperature: shape.temperature,
            stream: Some(false),
            stream_options: None,
            reasoning_effort: self.reasoning_effort_for_model(model),
            tool_stream: self.tool_stream_for_tools(has_tool_entries),
            tools,
            tool_choice: has_tool_entries.then(|| "auto".to_string()),
            max_tokens: shape.max_tokens,
            extra_body: shape.extra_body,
        }
    }

    /// Thinking params for a STREAMING request body. Passthrough never
    /// attaches thinking to the streamed wire: gateway SSE thinking frames
    /// are unverified territory (the live probe was non-streaming), and a
    /// streamed response cannot feed signed capture. This is defense in
    /// depth for a wrapper that streams this leaf despite the
    /// [`ModelProvider::supports_streaming`] disclaimer: such a request
    /// behaves like thinking-off for that turn instead of producing an
    /// uncapturable trajectory. Flag off keeps legacy streaming untouched.
    fn streaming_thinking_params(
        &self,
        thinking: Option<zeroclaw_api::model_provider::NativeThinkingParams>,
    ) -> Option<zeroclaw_api::model_provider::NativeThinkingParams> {
        if self.thinking_passthrough {
            None
        } else {
            thinking
        }
    }

    /// Streaming counterpart of [`Self::build_native_tool_chat_request`],
    /// used by `stream_chat` when native tools are present.
    fn build_streaming_native_tool_request(
        &self,
        model: &str,
        effective_messages: &[ChatMessage],
        tools: Option<Vec<NativeToolSpec>>,
        temperature: Option<f64>,
        options_enabled: bool,
        merge: bool,
        system_merged: bool,
        thinking: Option<zeroclaw_api::model_provider::NativeThinkingParams>,
    ) -> NativeChatRequest {
        // Guard on the converted tools being non-empty (not just the raw
        // input being non-empty): convert_tool_specs_for_model can sanitize
        // a non-empty input down to None, and tool_choice without a tools
        // field is an HTTP 400 on vLLM 0.19+. Computed before `tools` moves
        // into the request so the converted list is never copied.
        let tool_choice = tools
            .as_ref()
            .and_then(|t| (!t.is_empty()).then(|| "auto".to_string()));
        let mut messages = self.convert_messages_for_native(effective_messages, !merge);
        let carrier = Self::merged_system_carrier_index(&messages, system_merged);
        self.apply_cache_breakpoints(&mut messages, carrier);
        // Streamed requests are thinking-off under passthrough (see
        // streaming_thinking_params). History replay blocks require the
        // request thinking object, so strip them here: a hypothetical
        // wrapper streaming this leaf gets a thinking-off request, not a
        // blocks-without-thinking-object 400. Stripping after the cache
        // breakpoints are placed keeps both features composing on one
        // converted message list.
        if self.thinking_passthrough {
            for message in &mut messages {
                message.thinking_blocks = None;
            }
        }
        let shape = self.resolve_request_shape(
            model,
            self.streaming_thinking_params(thinking),
            temperature,
            self.max_tokens,
        );
        NativeChatRequest {
            model: model.to_string(),
            messages,
            temperature: shape.temperature,
            reasoning_effort: self.reasoning_effort_for_model(model),
            tool_stream: if options_enabled {
                self.tool_stream_for_tools(true)
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
            tools,
            tool_choice,
            max_tokens: shape.max_tokens,
            extra_body: shape.extra_body,
        }
    }

    async fn normalize_messages_for_upstream(
        &self,
        messages: &[ChatMessage],
    ) -> anyhow::Result<Vec<ChatMessage>> {
        let config = self.multimodal.clone();
        let sanitized;
        let messages = if self.tool_result_image_policy == ToolResultImagePolicy::Omit {
            sanitized = messages
                .iter()
                .map(|message| {
                    if message.role == "tool" {
                        ChatMessage {
                            role: message.role.clone(),
                            content: Self::sanitize_tool_result_message(&message.content),
                        }
                    } else {
                        message.clone()
                    }
                })
                .collect::<Vec<_>>();
            sanitized.as_slice()
        } else {
            messages
        };
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
        Self::content_with_image_parts(content)
    }

    fn content_with_image_parts(content: &str) -> MessageContent {
        let (cleaned_text, image_refs) = multimodal::parse_image_markers(content);
        if image_refs.is_empty() {
            return MessageContent::Text(content.to_string());
        }

        let mut parts = Vec::with_capacity(image_refs.len() + 1);
        let trimmed_text = cleaned_text.trim();
        if !trimmed_text.is_empty() {
            parts.push(MessagePart::Text {
                text: trimmed_text.to_string(),
                cache_control: None,
            });
        }

        for image_ref in image_refs {
            parts.push(MessagePart::ImageUrl {
                image_url: ImageUrlPart { url: image_ref },
            });
        }

        MessageContent::Parts(parts)
    }

    fn sanitize_tool_result_content(content: &str) -> String {
        let mut cleaned = String::with_capacity(content.len());
        let mut cursor = 0;
        let mut removed_image_marker = false;

        while let Some(relative_start) = content[cursor..].find("[IMAGE:") {
            let start = cursor + relative_start;
            cleaned.push_str(&content[cursor..start]);
            removed_image_marker = true;

            let after_prefix = start + "[IMAGE:".len();
            cursor = content[after_prefix..]
                .find(']')
                .map(|relative_end| after_prefix + relative_end + 1)
                .unwrap_or(content.len());
            if cursor == content.len() {
                break;
            }
        }

        cleaned.push_str(&content[cursor..]);
        if !removed_image_marker {
            return content.to_string();
        }

        if !cleaned.is_empty() {
            cleaned.push_str("\n\n");
        }
        cleaned.push_str(TOOL_RESULT_IMAGE_OMITTED_NOTICE);
        cleaned
    }

    fn sanitize_tool_result_message(content: &str) -> String {
        if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(content)
            && let Some(tool_content) = value.get_mut("content")
        {
            let raw_content = tool_content
                .as_str()
                .map(ToString::to_string)
                .unwrap_or_else(|| tool_content.to_string());
            let sanitized_content = Self::sanitize_tool_result_content(&raw_content);
            if sanitized_content == raw_content {
                return content.to_string();
            }
            *tool_content = serde_json::Value::String(sanitized_content);
            return value.to_string();
        }

        Self::sanitize_tool_result_content(content)
    }

    fn message_content_for_role(
        &self,
        role: &str,
        content: &str,
        allow_user_image_parts: bool,
        allow_tool_image_parts: bool,
    ) -> MessageContent {
        if role == "tool" {
            if self.tool_result_image_policy == ToolResultImagePolicy::Omit {
                return MessageContent::Text(Self::sanitize_tool_result_content(content));
            }
            if allow_tool_image_parts && allow_user_image_parts {
                return Self::content_with_image_parts(content);
            }
            return MessageContent::Text(content.to_string());
        }
        Self::to_message_content(role, content, allow_user_image_parts)
    }

    fn convert_messages_for_native(
        &self,
        messages: &[ChatMessage],
        allow_user_image_parts: bool,
    ) -> Vec<NativeMessage> {
        let targets_mistral_tool_call_contract = self.targets_mistral_tool_call_contract();
        let requires_string_tool_call_content = self.requires_string_tool_call_content();
        let mut used_tool_call_ids = std::collections::HashSet::new();
        let mut tool_call_id_map = std::collections::HashMap::new();
        let mut last_assistant_tool_call_ids: Vec<String> = Vec::new();
        let mut tool_name_map = std::collections::HashMap::new();

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
                        .map(|tc| {
                            let tc_id = tc.id.clone();
                            let tc_name = tc.name.clone();
                            tool_name_map.insert(tc_id, tc_name);
                            ToolCall {
                                id: Some({
                                    let normalized_id = reserve_tool_call_id_for_contract(
                                        targets_mistral_tool_call_contract,
                                        Some(tc.id.clone()),
                                        &mut used_tool_call_ids,
                                    );
                                    tool_call_id_map.insert(tc.id.clone(), normalized_id.clone());
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
                            }
                        })
                        .collect::<Vec<_>>();

                    last_assistant_tool_call_ids =
                        tool_calls.iter().filter_map(|tc| tc.id.clone()).collect();

                    let content = crate::request_payload::non_empty_string_field(&value, "content")
                        .map(MessageContent::Text)
                        .or_else(|| {
                            requires_string_tool_call_content
                                .then(|| MessageContent::Text(String::new()))
                        });

                    let replay = self.assistant_thinking_replay(&value);

                    return NativeMessage {
                        role: "assistant".to_string(),
                        content,
                        tool_call_id: None,
                        tool_calls: Some(tool_calls),
                        reasoning_content: replay.reasoning_content,
                        reasoning: replay.reasoning,
                        thinking_blocks: replay.thinking_blocks,
                        name: None,
                    };
                }

                if message.role == "assistant"
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content)
                    && value.get("tool_calls").is_none()
                    && Self::assistant_reasoning_value(&value).is_some()
                    && matches!(
                        value.get("content"),
                        None | Some(serde_json::Value::Null | serde_json::Value::String(_))
                    )
                {
                    let content = value
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .map(|value| MessageContent::Text(value.to_string()));

                    let replay = self.assistant_thinking_replay(&value);

                    return NativeMessage {
                        role: "assistant".to_string(),
                        content,
                        tool_call_id: None,
                        tool_calls: None,
                        reasoning_content: replay.reasoning_content,
                        reasoning: replay.reasoning,
                        thinking_blocks: replay.thinking_blocks,
                        name: None,
                    };
                }

                if message.role == "tool"
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content)
                {
                    let mut tool_call_id = value
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
                    // Fallback: if the tool result JSON dropped the tool_call_id,
                    // borrow the first id from the most recent assistant message.
                    // Some multi-turn reconstruction paths strip this field, and
                    // strict backends (Groq, Mistral) reject null/missing ids.
                    if tool_call_id.is_none() && !last_assistant_tool_call_ids.is_empty() {
                        tool_call_id = last_assistant_tool_call_ids.first().cloned();
                    }
                    let content = value
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .map(|value| {
                            self.message_content_for_role(
                                "tool",
                                value,
                                allow_user_image_parts,
                                true,
                            )
                        })
                        .or_else(|| {
                            Some(self.message_content_for_role(
                                "tool",
                                &message.content,
                                allow_user_image_parts,
                                false,
                            ))
                        });

                    // Groq native tool calling requires the tool `name` on
                    // every role-tool message; look it up from the paired
                    // assistant tool-call, falling back to any name carried
                    // in the tool message content itself./
                    let tool_name = value
                        .get("tool_call_id")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|raw_id| tool_name_map.get(raw_id).cloned())
                        .or_else(|| {
                            value
                                .get("name")
                                .and_then(serde_json::Value::as_str)
                                .map(ToString::to_string)
                        });

                    return NativeMessage {
                        role: "tool".to_string(),
                        content,
                        tool_call_id,
                        tool_calls: None,
                        reasoning_content: None,
                        reasoning: None,
                        thinking_blocks: None,
                        name: tool_name,
                    };
                }

                NativeMessage {
                    role: message.role.clone(),
                    content: Some(self.message_content_for_role(
                        &message.role,
                        &message.content,
                        allow_user_image_parts,
                        false,
                    )),
                    tool_call_id: None,
                    tool_calls: None,
                    reasoning_content: None,
                    reasoning: None,
                    thinking_blocks: None,
                    name: None,
                }
            })
            .collect()
    }

    fn strip_native_tool_messages(&self, messages: &[ChatMessage]) -> Vec<ChatMessage> {
        if self.native_tool_calling {
            return messages.to_vec();
        }
        let intermediate = messages.iter().enumerate().filter_map(|(index, msg)| {
            if ChatMessage::should_skip_internal_pruning_marker(messages, index) {
                return None;
            }
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

        let mut coalesced: Vec<ChatMessage> = Vec::with_capacity(messages.len());
        for msg in intermediate {
            match coalesced.last_mut() {
                Some(last) if last.role == msg.role && msg.role != "system" => {
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

    /// Whether this backend requires `content` to be a string on assistant
    /// tool-call messages.
    ///
    /// OpenAI accepts the field absent or null there, and omitting it is the
    /// default. Cloudflare Workers AI validates against a stricter schema and
    /// rejects the whole request with HTTP 400 (`AiError: Bad input ...
    /// required properties at '/messages/N' are 'role,content'`). The failure
    /// is intermittent in practice: a model that emits text alongside its tool
    /// call produces a non-empty content and succeeds, while the far more
    /// common no-text tool call fails.
    fn requires_string_tool_call_content(&self) -> bool {
        reqwest::Url::parse(&self.base_url)
            .ok()
            .and_then(|url| url.host_str().map(|h| h.to_ascii_lowercase()))
            .is_some_and(|host| {
                host == "api.cloudflare.com"
                    || host == "gateway.ai.cloudflare.com"
                    || host.ends_with(".cloudflare.com")
            })
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
        let reasoning_content = self.capture_reasoning_content(&message);
        let mut used_tool_call_ids = std::collections::HashSet::new();
        let tool_calls = message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .filter_map(|tc| {
                let name = tc.function_name()?;
                let arguments = tc.function_arguments().unwrap_or_else(|| "{}".to_string());
                let normalized_arguments = sanitize_tool_arguments(&name, &arguments);
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

    /// Flag-gated reasoning capture for a parsed gateway message.
    ///
    /// Flag off: today's behavior exactly — the `reasoning_content` string is
    /// passed through untouched (DeepSeek/GLM/Qwen flows).
    ///
    /// Flag on: normalize into the newline-delimited signed-JSON line format
    /// the native Anthropic provider uses for `reasoning_content` (one
    /// `{"thinking":..,"signature":..}` line per block). A `thinking_blocks`
    /// array (signed) wins over a plain reasoning string; a bare string is
    /// wrapped as a single unsigned line (`signature: ""`), matching how the
    /// native provider emits signature-less blocks.
    fn capture_reasoning_content(&self, message: &ResponseMessage) -> Option<String> {
        if !self.thinking_passthrough {
            return message.reasoning_content.clone();
        }
        if let Some(blocks) = &message.thinking_blocks
            && let Some(lines) = Self::thinking_blocks_to_reasoning_lines(blocks)
        {
            return Some(lines);
        }
        message.reasoning_content.as_ref().map(|reasoning| {
            serde_json::json!({"thinking": reasoning, "signature": ""}).to_string()
        })
    }

    /// Convert gateway `thinking_blocks` (Anthropic content-block style) into
    /// the native signed-JSON line format. Blocks with neither thinking text
    /// nor a signature are skipped. Returns `None` when no block survives, so
    /// the caller falls back to the plain reasoning string.
    ///
    /// Deliberately mirrors the native Anthropic provider's block emission
    /// (`crates/zeroclaw-providers/src/anthropic.rs`): same line shape, same
    /// signature-or-text inclusion rule. Duplicate-with-comment, not a shared
    /// helper, per the passthrough spec boundary (mirrors the native provider).
    ///
    /// Block-type whitelist: only `thinking` and `redacted_thinking` are
    /// captured. `redacted_thinking` is preserved verbatim (it must replay
    /// unchanged and carries no signature by design) but only when its
    /// required opaque `data` field is a non-empty string. Every other type
    /// is skipped, not stored: accepting gateway-controlled JSON blocks of
    /// unknown shape into replay history would be an unvalidated
    /// network-to-request channel. Skipping keeps a novel-but-harmless block
    /// type from bricking the turn.
    fn thinking_blocks_to_reasoning_lines(blocks: &[serde_json::Value]) -> Option<String> {
        let mut lines: Vec<String> = Vec::with_capacity(blocks.len());
        for block in blocks {
            let block_type = block
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("thinking");
            if block_type == "redacted_thinking" {
                let has_data = block
                    .get("data")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|data| !data.is_empty());
                if has_data {
                    lines.push(block.to_string());
                }
                continue;
            }
            if block_type != "thinking" {
                continue;
            }
            let thinking = block
                .get("thinking")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let signature = block
                .get("signature")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if thinking.is_empty() && signature.is_empty() {
                continue;
            }
            lines.push(
                serde_json::json!({"thinking": thinking, "signature": signature}).to_string(),
            );
        }
        (!lines.is_empty()).then(|| lines.join("\n"))
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
    fn default_base_url(&self) -> Option<&str> {
        self.canonical_base_url
    }

    fn capabilities(&self) -> zeroclaw_api::model_provider::ProviderCapabilities {
        zeroclaw_api::model_provider::ProviderCapabilities {
            native_tool_calling: self.native_tool_calling,
            vision: self.supports_vision,
            prompt_caching: self.cache_passthrough,
            extended_thinking: false,
        }
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        // When a credential is present, hit the model_provider's native /models endpoint
        // (OpenAI-compatible: GET {base_url}/models). Local OpenAI-compatible
        // servers with a public catalog use the same path without an Authorization header.
        // A profile that authenticates purely through `extra_headers` (e.g. a
        // `Cookie` or `X-Auth` bridge, rather than a credential resolved into
        // `Authorization`) must be probed too — otherwise its configured
        // endpoint and any real auth failure are never observed, and the
        // caller silently falls back to an unrelated public catalog.
        let list_credential = self.resolve_credential().await?;
        if list_credential.is_some() || self.public_model_listing || !self.extra_headers.is_empty()
        {
            let url = self.models_url();
            // A configured endpoint URL can carry credentials in its userinfo,
            // query, or fragment. Log and report only the scrubbed form: the
            // central catalog caller sanitizes the returned error, but these
            // structured log attributes and error strings are produced before
            // it and would otherwise leak into operator logs.
            let safe_url = super::sanitize_api_error(&url);
            let response = self
                .apply_auth_header(self.http_client().get(&url), list_credential.as_deref())?
                .send()
                .await
                .map_err(|e| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "model_provider": &self.name,
                                "url": &safe_url,
                                "phase": "model_list_request",
                                "error": super::format_error_chain(&e),
                            })),
                        "compatible: model list request failed"
                    );
                    anyhow::Error::msg(format!(
                        "{} model list request failed: {safe_url}: {e}",
                        self.name
                    ))
                })?;
            if !response.status().is_success() {
                let status = response.status();
                anyhow::bail!(
                    "{} model list failed at {safe_url}: HTTP {status}",
                    self.name
                );
            }
            let raw = read_body_capped(response, MAX_MODELS_RESPONSE_BYTES)
                .await
                .map_err(|e| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "model_provider": &self.name,
                                "phase": "model_list_read",
                                "error": format!("{e:#}"),
                            })),
                        "compatible: model list body was too large or could not be read"
                    );
                    anyhow::Error::msg(format!(
                        "{} model list body was not readable: {e}",
                        self.name
                    ))
                })?;
            let body: ModelsResponse = serde_json::from_slice(&raw).map_err(|e| {
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
            None => Err(zeroclaw_api::model_provider::ModelListingUnsupportedError.into()),
        }
    }

    async fn list_models_with_pricing(
        &self,
    ) -> anyhow::Result<Vec<zeroclaw_api::model_provider::ModelInfo>> {
        // When a credential is present, hit the provider's native /models
        // endpoint — this returns pricing data that we can capture. A
        // header-only authenticated profile (see `list_models` above) is
        // probed too, for the same reason.
        let list_credential = self.resolve_credential().await?;
        if list_credential.is_some() || self.public_model_listing || !self.extra_headers.is_empty()
        {
            let url = self.models_url();
            // A configured endpoint URL can carry credentials in its userinfo,
            // query, or fragment. Log and report only the scrubbed form: the
            // central catalog caller sanitizes the returned error, but these
            // structured log attributes and error strings are produced before
            // it and would otherwise leak into operator logs.
            let safe_url = super::sanitize_api_error(&url);
            let response = self
                .apply_auth_header(self.http_client().get(&url), list_credential.as_deref())?
                .send()
                .await
                .map_err(|e| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "model_provider": &self.name,
                                "url": &safe_url,
                                "phase": "model_list_request",
                                "error": super::format_error_chain(&e),
                            })),
                        "compatible: model list request failed"
                    );
                    anyhow::Error::msg(format!(
                        "{} model list request failed: {safe_url}: {e}",
                        self.name
                    ))
                })?;
            if !response.status().is_success() {
                let status = response.status();
                anyhow::bail!(
                    "{} model list failed at {safe_url}: HTTP {status}",
                    self.name
                );
            }
            let raw = read_body_capped(response, MAX_MODELS_RESPONSE_BYTES)
                .await
                .map_err(|e| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "model_provider": &self.name,
                                "phase": "model_list_read",
                                "error": format!("{e:#}"),
                            })),
                        "compatible: model list body was too large or could not be read"
                    );
                    anyhow::Error::msg(format!(
                        "{} model list body was not readable: {e}",
                        self.name
                    ))
                })?;
            let body: ModelsResponse = serde_json::from_slice(&raw).map_err(|e| {
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
            match crate::models_dev::list_models_with_context_for(key).await {
                Ok(models) if !models.is_empty() => {
                    return Ok(models_dev_to_model_info(models));
                }
                Ok(_) => {} // empty → fall through to openrouter
                Err(error) if self.openrouter_vendor_prefix.is_none() => {
                    return Err(error);
                }
                Err(_) => {} // fall through to openrouter
            }
        }
        match &self.openrouter_vendor_prefix {
            Some(prefix) => {
                crate::openrouter_catalog::list_models_for_vendor_with_pricing(prefix).await
            }
            None => Err(zeroclaw_api::model_provider::ModelListingUnsupportedError.into()),
        }
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let credential = self.resolve_credential().await?;

        // Normalize image markers (e.g. local file paths from channel
        // attachments) into base64 data URIs before this message reaches the
        // upstream provider.
        let user_msg = ChatMessage {
            role: "user".to_string(),
            content: message.to_string(),
        };
        let normalized_user = self
            .normalize_messages_for_upstream(std::slice::from_ref(&user_msg))
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
                thinking_blocks: None,
            });
        } else {
            if let Some(sys) = system_prompt {
                messages.push(Message {
                    role: "system".to_string(),
                    content: MessageContent::Text(sys.to_string()),
                    thinking_blocks: None,
                });
            }
            messages.push(Message {
                role: "user".to_string(),
                content: Self::to_message_content("user", &normalized_message, true),
                thinking_blocks: None,
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
            extra_body: self.extra_body.clone(),
        };
        // No cache breakpoints here, deliberately: this text-only helper
        // never surfaces response usage to dispatch accounting, so a
        // premium cache write it triggered could never be accounted for
        // (fallback re-entries through this path return `usage: None`).
        // The flag is honored on the structured paths (`chat`,
        // `chat_with_tools`, `stream_chat`), which capture usage; the
        // provider docs describe this usage-capture boundary.

        let url = self.chat_completions_url();

        let response = match self
            .apply_opencode_session_header(self.apply_auth_header(
                self.http_client().post(&url).json(&request),
                credential.as_deref(),
            )?)
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
        let (message, _usage) = self
            .chat_with_history_inner(messages, model, temperature, None)
            .await?;
        Ok(legacy_history_text(&message))
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ProviderChatResponse> {
        let credential = self.resolve_credential().await?;

        let normalized = self.normalize_messages_for_upstream(messages).await?;
        let merge = self.effective_merge_system(model);
        let (effective_messages, system_merged) = Self::flatten_system_messages(&normalized, merge);
        let effective_messages = if self.native_tool_calling {
            effective_messages
        } else {
            self.strip_native_tool_messages(&effective_messages)
        };
        let request = self.build_raw_native_tool_chat_request(
            &effective_messages,
            (!tools.is_empty()).then_some(tools),
            model,
            temperature,
            !merge,
            system_merged,
            // Legacy entry point: carries no `ChatRequest`, so no runtime
            // thinking params are available to forward.
            None,
        );
        let mut payload = serde_json::to_value(request)?;
        let tools_count = payload
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len);

        let url = self.chat_completions_url();
        let response = loop {
            let response = match self
                .apply_opencode_session_header(self.apply_auth_header(
                    self.http_client().post(&url).json(&payload),
                    credential.as_deref(),
                )?)
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
            if response.status().is_success() {
                break response;
            }

            let status = response.status();
            let error = response.text().await?;
            if tools_count > 0
                && super::rejects_tools_with_reasoning_effort(status, &error)
                && ensure_reasoning_effort_none(&mut payload)
            {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Retry)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "provider": &self.name,
                            "alias": &self.alias,
                            "request_api": "chat_completions",
                            "model": model,
                            "stream": false,
                            "tools_count": tools_count,
                            "reasoning_effort_overridden": true,
                            "reasoning_effort_fallback": "none",
                            "reasoning_effort_override_reason": "endpoint_rejected_tools_with_reasoning",
                            "status": status.as_u16(),
                        })),
                    "compatible provider retrying with reasoning effort disabled after endpoint capability rejection"
                );
                continue;
            }

            return Err(super::api_error_from_parts(&self.name, status, &error));
        };

        let body = response.text().await?;
        let chat_response = parse_chat_response_body(&self.name, &body)?;
        let usage = chat_response.usage.map(UsageInfo::into_provider_usage);
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

        let mut result = self.parse_native_response(choice.message);
        result.usage = usage;
        Ok(result)
    }

    async fn chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ProviderChatResponse> {
        let credential = self.resolve_credential().await?;

        let normalized = self
            .normalize_messages_for_upstream(request.messages)
            .await?;
        let merge = self.effective_merge_system(model);
        let (effective_messages, system_merged) = Self::flatten_system_messages(&normalized, merge);
        let effective_messages = if self.native_tool_calling {
            effective_messages
        } else {
            self.strip_native_tool_messages(&effective_messages)
        };

        let tools = self.convert_tool_specs_for_model(request.tools, model);
        let native_request = self.build_native_tool_chat_request(
            &effective_messages,
            tools,
            model,
            temperature,
            !merge,
            system_merged,
            request.thinking,
        );
        let mut payload = serde_json::to_value(native_request)?;
        let tools_count = payload
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len);
        let reasoning_effort_omitted =
            self.reasoning_effort.is_some() && payload.get("reasoning_effort").is_none();
        let reasoning_effort_omission_reason =
            reasoning_effort_omitted.then_some("model_ineligible");
        if ::zeroclaw_log::debug_enabled() {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                    .with_attrs(::serde_json::json!({
                        "provider": &self.name,
                        "alias": &self.alias,
                        "request_api": "chat_completions",
                        "model": model,
                        "stream": false,
                        "native_tool_calling": self.native_tool_calling,
                        "tools_count": tools_count,
                        "tool_choice": payload.get("tool_choice"),
                        "reasoning_effort_omitted": reasoning_effort_omitted,
                        "reasoning_effort_omission_reason": reasoning_effort_omission_reason,
                    })),
                "compatible provider request prepared"
            );
        }

        let url = self.chat_completions_url();
        let response = loop {
            let response = match self
                .apply_opencode_session_header(self.apply_auth_header(
                    self.http_client().post(&url).json(&payload),
                    credential.as_deref(),
                )?)
                .send()
                .await
            {
                Ok(response) => response,
                Err(chat_error) => return Err(chat_error.into()),
            };
            if response.status().is_success() {
                break response;
            }

            let status = response.status();
            let error = response.text().await?;
            let sanitized = super::sanitize_api_error(&error);

            if tools_count > 0
                && super::rejects_tools_with_reasoning_effort(status, &error)
                && ensure_reasoning_effort_none(&mut payload)
            {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Retry)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "provider": &self.name,
                            "alias": &self.alias,
                            "request_api": "chat_completions",
                            "model": model,
                            "stream": false,
                            "tools_count": tools_count,
                            "reasoning_effort_overridden": true,
                            "reasoning_effort_fallback": "none",
                            "reasoning_effort_override_reason": "endpoint_rejected_tools_with_reasoning",
                            "status": status.as_u16(),
                        })),
                    "compatible provider retrying with reasoning effort disabled after endpoint capability rejection"
                );
                continue;
            }

            if Self::is_native_tool_schema_unsupported(status, &sanitized) {
                let fallback_messages =
                    Self::with_prompt_guided_tool_instructions(request.messages, request.tools);
                let (message, usage) = self
                    .chat_with_history_inner(
                        &fallback_messages,
                        model,
                        temperature,
                        request.thinking,
                    )
                    .await?;
                let mut response = self.parse_native_response(message);
                response.usage = usage;
                return Ok(response);
            }

            anyhow::bail!("{} API error ({status}): {sanitized}", self.name);
        };

        let body = response.text().await?;
        let native_response = parse_chat_response_body(&self.name, &body)?;
        let usage = native_response.usage.map(UsageInfo::into_provider_usage);
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
        // Passthrough pins requests to the non-streaming wire: gateway SSE
        // thinking frames are unverified territory (the live probe was
        // non-streaming), and streamed reasoning would reach history
        // unsigned, breaking signed block replay on the next tool-loop
        // iteration. The flag is opt-in, so trading streaming for a correct
        // signed trajectory is the honest default until wire-first
        // streaming support lands.
        !self.thinking_passthrough
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

        let provider = self.clone();
        // Resolved here, not in the spawned task: `spawn!` propagates the
        // tracing span but not task-locals, so the conversation scope is
        // unreadable past this point.
        let opencode_session = self.opencode_session_value();
        let messages_owned: Vec<ChatMessage> = request.messages.to_vec();
        let tools_owned: Option<Vec<zeroclaw_api::tool::ToolSpec>> =
            request.tools.map(<[zeroclaw_api::tool::ToolSpec]>::to_vec);
        // `NativeThinkingParams` is `Copy`; captured for both streaming
        // request branches (tool and tool-less) below.
        let thinking_owned = request.thinking;
        let model = model.to_string();
        let count_tokens = options.count_tokens;
        let options_enabled = options.enabled;

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(100);

        let handle = ::zeroclaw_spawn::spawn!(async move {
            let normalized = match provider
                .normalize_messages_for_upstream(&messages_owned)
                .await
            {
                Ok(n) => n,
                Err(err) => {
                    let _ = tx
                        .send(Err(StreamError::ModelProvider(err.to_string())))
                        .await;
                    return;
                }
            };

            let merge = provider.effective_merge_system(&model);
            let (effective_messages, system_merged) =
                Self::flatten_system_messages(&normalized, merge);
            let effective_messages = provider.strip_native_tool_messages(&effective_messages);
            let tools = provider.convert_tool_specs_for_model(tools_owned.as_deref(), &model);
            let tools_count = tools.as_ref().map_or(0, Vec::len);
            let has_tools = tools_count > 0;
            let reasoning_effort = provider.reasoning_effort_for_model(&model);
            let reasoning_effort_omitted =
                provider.reasoning_effort.is_some() && reasoning_effort.is_none();
            let reasoning_effort_omission_reason =
                reasoning_effort_omitted.then_some("model_ineligible");

            let payload_result = if has_tools {
                serde_json::to_value(provider.build_streaming_native_tool_request(
                    &model,
                    &effective_messages,
                    tools,
                    temperature,
                    options_enabled,
                    merge,
                    system_merged,
                    thinking_owned,
                ))
            } else {
                let mut messages: Vec<Message> = effective_messages
                    .iter()
                    .map(|message| Message {
                        role: message.role.clone(),
                        content: provider.message_content_for_role(
                            &message.role,
                            &message.content,
                            !merge,
                            false,
                            // Streamed requests are thinking-off under
                            // passthrough (see streaming_thinking_params):
                            // history replay blocks require the request
                            // thinking object, so they never ride the
                            // streamed wire.
                        ),
                        thinking_blocks: None,
                    })
                    .collect();
                let carrier = Self::merged_system_carrier_index(&messages, system_merged);
                provider.apply_cache_breakpoints(&mut messages, carrier);

                let shape = provider.resolve_request_shape(
                    &model,
                    provider.streaming_thinking_params(thinking_owned),
                    temperature,
                    provider.max_tokens,
                );
                serde_json::to_value(ApiChatRequest {
                    model: model.clone(),
                    messages,
                    temperature: shape.temperature,
                    reasoning_effort: reasoning_effort.clone(),
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
                    max_tokens: shape.max_tokens,
                    extra_body: shape.extra_body,
                })
            };

            let mut payload = match payload_result {
                Ok(payload) => payload,
                Err(error) => {
                    let _ = tx.send(Err(StreamError::Json(error))).await;
                    return;
                }
            };
            if ::zeroclaw_log::debug_enabled() {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                        .with_attrs(::serde_json::json!({
                            "provider": &provider.name,
                            "alias": &provider.alias,
                            "request_api": "chat_completions",
                            "model": &model,
                            "stream": options_enabled,
                            "native_tool_calling": provider.native_tool_calling,
                            "tools_count": tools_count,
                            "tool_choice": payload.get("tool_choice"),
                            "reasoning_effort_omitted": reasoning_effort_omitted,
                            "reasoning_effort_omission_reason": reasoning_effort_omission_reason,
                        })),
                    "compatible streaming provider request prepared"
                );
            }

            let url = provider.chat_completions_url();
            let client = provider.streaming_http_client();
            let idle_timeout = super::stream_idle_timeout(provider.timeout_secs);
            let auth_header = provider.auth_header.clone();
            let credential = match provider.resolve_credential().await {
                Ok(credential) => credential,
                Err(error) => {
                    let _ = tx
                        .send(Err(StreamError::ModelProvider(error.to_string())))
                        .await;
                    return;
                }
            };
            let targets_mistral_tool_call_contract = provider.targets_mistral_tool_call_contract();

            let response = loop {
                let mut req_builder = client.post(&url).json(&payload);
                req_builder = match apply_auth_to_request(
                    req_builder,
                    &auth_header,
                    credential.as_deref(),
                    &provider.name,
                ) {
                    Ok(req_builder) => req_builder,
                    Err(error) => {
                        let _ = tx
                            .send(Err(StreamError::ModelProvider(error.to_string())))
                            .await;
                        return;
                    }
                };
                req_builder = req_builder.header("Accept", "text/event-stream");
                if let Some(session) = opencode_session.as_deref() {
                    req_builder = req_builder.header(OPENCODE_SESSION_HEADER, session);
                }

                let response = match req_builder.send().await {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = tx
                            .send(Err(StreamError::Http(super::stream_idle_error_message(
                                &e,
                                idle_timeout,
                            ))))
                            .await;
                        return;
                    }
                };
                if response.status().is_success() {
                    break response;
                }

                let status = response.status();
                let error = match response.text().await {
                    Ok(text) => text,
                    Err(_) => format!("HTTP error: {}", status),
                };
                if tools_count > 0
                    && super::rejects_tools_with_reasoning_effort(status, &error)
                    && ensure_reasoning_effort_none(&mut payload)
                {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Retry)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "provider": &provider.name,
                                "alias": &provider.alias,
                                "request_api": "chat_completions",
                                "model": &model,
                                "stream": options_enabled,
                                "tools_count": tools_count,
                                "reasoning_effort_overridden": true,
                                "reasoning_effort_fallback": "none",
                                "reasoning_effort_override_reason": "endpoint_rejected_tools_with_reasoning",
                                "status": status.as_u16(),
                            })),
                        "compatible streaming provider retrying with reasoning effort disabled after endpoint capability rejection"
                    );
                    continue;
                }

                let _ = tx.send(Err(streaming_api_error(status, &error))).await;
                return;
            };

            let mut event_stream = sse_bytes_to_events_for_contract(
                response,
                count_tokens,
                targets_mistral_tool_call_contract,
                idle_timeout,
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
        let provider = self.clone();
        // Resolved here, not in the spawned task: `spawn!` propagates the
        // tracing span but not task-locals, so the conversation scope is
        // unreadable past this point.
        let opencode_session = self.opencode_session_value();
        let system_prompt_owned: Option<String> = system_prompt.map(str::to_string);
        let message_owned = message.to_string();
        let model = model.to_string();
        let count_tokens = options.count_tokens;
        let options_enabled = options.enabled;

        // Use a channel to bridge the async HTTP response to the stream
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamChunk>>(100);

        let handle = ::zeroclaw_spawn::spawn!(async move {
            // Normalize image markers in the user-supplied message before
            // forwarding upstream — seefor the OpenAI-compatible
            // remote-vs-local file path problem.
            let user_msg = ChatMessage {
                role: "user".to_string(),
                content: message_owned,
            };
            let normalized_user = match provider
                .normalize_messages_for_upstream(std::slice::from_ref(&user_msg))
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
                    thinking_blocks: None,
                });
            } else {
                if let Some(sys) = system_prompt_owned {
                    messages.push(Message {
                        role: "system".to_string(),
                        content: MessageContent::Text(sys),
                        thinking_blocks: None,
                    });
                }
                messages.push(Message {
                    role: "user".to_string(),
                    content: Self::to_message_content("user", &normalized_message_content, !merge),
                    thinking_blocks: None,
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
                extra_body: provider.extra_body.clone(),
            };
            // No cache breakpoints here, deliberately: this legacy chunk
            // stream never converts the final usage chunk into a
            // `TokenUsage`, so a premium cache write it triggered could
            // never be accounted for. The flag is honored on the structured
            // streaming path (`stream_chat`), which emits
            // `StreamEvent::Usage`.

            let url = provider.chat_completions_url();
            let client = provider.streaming_http_client();
            let idle_timeout = super::stream_idle_timeout(provider.timeout_secs);
            let auth_header = provider.auth_header.clone();
            let credential = match provider.resolve_credential().await {
                Ok(credential) => credential,
                Err(error) => {
                    let _ = tx
                        .send(Err(StreamError::ModelProvider(error.to_string())))
                        .await;
                    return;
                }
            };

            // Build request with auth
            let mut req_builder = client.post(&url).json(&request);

            // Apply auth header, or refuse locally if the credential cannot be
            // turned into one.
            req_builder = match apply_auth_to_request(
                req_builder,
                &auth_header,
                credential.as_deref(),
                &provider.name,
            ) {
                Ok(req_builder) => req_builder,
                Err(error) => {
                    let _ = tx
                        .send(Err(StreamError::ModelProvider(error.to_string())))
                        .await;
                    return;
                }
            };

            // Set accept header for streaming
            req_builder = req_builder.header("Accept", "text/event-stream");
            if let Some(session) = opencode_session.as_deref() {
                req_builder = req_builder.header(OPENCODE_SESSION_HEADER, session);
            }

            // Send request
            let response = match req_builder.send().await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx
                        .send(Err(StreamError::Http(super::stream_idle_error_message(
                            &e,
                            idle_timeout,
                        ))))
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
                let _ = tx.send(Err(streaming_api_error(status, &error))).await;
                return;
            }

            // Convert to chunk stream and forward to channel
            let mut chunk_stream = sse_bytes_to_chunks(response, count_tokens, idle_timeout);
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
        let provider = self.clone();
        // Resolved here, not in the spawned task: `spawn!` propagates the
        // tracing span but not task-locals, so the conversation scope is
        // unreadable past this point.
        let opencode_session = self.opencode_session_value();
        let messages_owned: Vec<ChatMessage> = messages.to_vec();
        let model = model.to_string();
        let count_tokens = options.count_tokens;
        let options_enabled = options.enabled;

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamChunk>>(100);

        let handle = ::zeroclaw_spawn::spawn!(async move {
            let normalized = match provider
                .normalize_messages_for_upstream(&messages_owned)
                .await
            {
                Ok(n) => n,
                Err(err) => {
                    let _ = tx
                        .send(Err(StreamError::ModelProvider(err.to_string())))
                        .await;
                    return;
                }
            };

            let merge = provider.effective_merge_system(&model);
            let (effective_messages, _system_merged) =
                Self::flatten_system_messages(&normalized, merge);
            let effective_messages = provider.strip_native_tool_messages(&effective_messages);
            let api_messages: Vec<Message> = effective_messages
                .iter()
                .map(|m| Message {
                    role: m.role.clone(),
                    content: provider.message_content_for_role(&m.role, &m.content, !merge, false),
                    thinking_blocks: None,
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
                extra_body: provider.extra_body.clone(),
            };
            // No cache breakpoints here, deliberately: this legacy chunk
            // stream never converts the final usage chunk into a
            // `TokenUsage`, so a premium cache write it triggered could
            // never be accounted for. The flag is honored on the structured
            // streaming path (`stream_chat`), which emits
            // `StreamEvent::Usage`.

            let url = provider.chat_completions_url();
            let client = provider.streaming_http_client();
            let idle_timeout = super::stream_idle_timeout(provider.timeout_secs);
            let auth_header = provider.auth_header.clone();
            let credential = match provider.resolve_credential().await {
                Ok(credential) => credential,
                Err(error) => {
                    let _ = tx
                        .send(Err(StreamError::ModelProvider(error.to_string())))
                        .await;
                    return;
                }
            };

            let mut req_builder = client.post(&url).json(&request);
            req_builder = match apply_auth_to_request(
                req_builder,
                &auth_header,
                credential.as_deref(),
                &provider.name,
            ) {
                Ok(req_builder) => req_builder,
                Err(error) => {
                    let _ = tx
                        .send(Err(StreamError::ModelProvider(error.to_string())))
                        .await;
                    return;
                }
            };
            req_builder = req_builder.header("Accept", "text/event-stream");
            if let Some(session) = opencode_session.as_deref() {
                req_builder = req_builder.header(OPENCODE_SESSION_HEADER, session);
            }

            let response = match req_builder.send().await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx
                        .send(Err(StreamError::Http(super::stream_idle_error_message(
                            &e,
                            idle_timeout,
                        ))))
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
                let _ = tx.send(Err(streaming_api_error(status, &error))).await;
                return;
            }

            let mut chunk_stream = sse_bytes_to_chunks(response, count_tokens, idle_timeout);
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
        // Probe the catalog without invoking inference. Unsupported endpoints are
        // still useful for connection warmup, so do not reject HTTP error statuses.
        let url = self.models_url();
        let credential = self.resolve_credential().await?;
        let mut response = self
            .apply_auth_header(self.http_client().get(&url), credential.as_deref())?
            .send()
            .await?;
        // Drain without retaining the catalog so HTTP/1 connections can be reused.
        while response.chunk().await?.is_some() {}
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

    /// Empty / whitespace arguments must collapse to `"{}"` so OpenAI-style
    /// providers never see an invalid `tool_calls[].function.arguments`.
    #[test]
    fn sanitize_tool_arguments_empty_or_whitespace_becomes_empty_object() {
        assert_eq!(sanitize_tool_arguments("f", ""), "{}");
        assert_eq!(sanitize_tool_arguments("f", "   \n\t  "), "{}");
    }

    /// The `/models` success path buffers the whole body before parsing, so a
    /// misbehaving or compromised router could otherwise make the client hold
    /// an unbounded response — most exposed on the credential-free
    /// `PUBLIC_MODEL_LISTING` path. `read_body_capped` must refuse an oversized
    /// success body two ways: a declared `Content-Length` over the cap fails
    /// fast, and a chunked body with NO `Content-Length` fails as it grows.
    #[tokio::test]
    async fn read_body_capped_bounds_oversized_success_bodies() {
        use axum::Router;
        use axum::body::{Body, Bytes};
        use axum::routing::get;
        use tokio::net::TcpListener;

        const CAP: u64 = 1024;
        let under = vec![b'x'; 512];

        let under_route = under.clone();
        let app = Router::new()
            .route(
                "/under",
                get(move || {
                    let body = under_route.clone();
                    async move { body }
                }),
            )
            .route(
                // Honest Content-Length over the cap.
                "/declared_over",
                get(|| async { vec![b'x'; (CAP as usize) + 4096] }),
            )
            .route(
                // Eight 512-byte chunks (4096 > CAP) streamed with no
                // Content-Length, so only the running accumulator can catch it.
                "/streamed_over",
                get(|| async {
                    let chunks =
                        (0..8).map(|_| Ok::<_, std::io::Error>(Bytes::from(vec![b'x'; 512])));
                    Body::from_stream(futures_util::stream::iter(chunks))
                }),
            );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::new();
        let fetch = |path: &str| {
            let url = format!("http://{addr}{path}");
            let client = client.clone();
            async move { client.get(url).send().await.expect("fixture request") }
        };

        // Under the cap: accepted, bytes preserved exactly.
        assert_eq!(
            read_body_capped(fetch("/under").await, CAP)
                .await
                .expect("a body under the cap is accepted"),
            under
        );
        // Declared oversize: refused before buffering.
        assert!(
            read_body_capped(fetch("/declared_over").await, CAP)
                .await
                .is_err(),
            "a declared-oversize body must be refused"
        );
        // Streamed oversize, no Content-Length: refused as it grows.
        assert!(
            read_body_capped(fetch("/streamed_over").await, CAP)
                .await
                .is_err(),
            "a streamed body past the cap must be refused"
        );

        server.abort();
    }

    /// Well-formed JSON object returns untouched — only object-shaped arguments
    /// satisfy the strict-provider function-arguments contract.
    #[test]
    fn sanitize_tool_arguments_valid_json_is_passthrough() {
        let args = r#"{"path":"/tmp/x","recursive":true}"#;
        assert_eq!(sanitize_tool_arguments("file_read", args), args);
    }

    /// Non-object JSON values (null, array, string, number, boolean) are
    /// rejected to `"{}"` because strict providers require a JSON object for
    /// tool-call arguments.
    #[test]
    fn sanitize_tool_arguments_non_object_becomes_empty_object() {
        assert_eq!(sanitize_tool_arguments("f", "null"), "{}");
        assert_eq!(sanitize_tool_arguments("f", "[]"), "{}");
        assert_eq!(sanitize_tool_arguments("f", "42"), "{}");
        assert_eq!(sanitize_tool_arguments("f", "\"hello\""), "{}");
        assert_eq!(sanitize_tool_arguments("f", "true"), "{}");
    }

    /// Malformed arguments are dropped to `"{}"` so strict upstreams (Cohere,
    /// OpenInference, Nvidia via OpenRouter) no longer reject the whole request
    /// with HTTP 400 just because the model emitted junk arguments.
    #[test]
    fn sanitize_tool_arguments_invalid_json_becomes_empty_object() {
        // Unterminated string
        assert_eq!(sanitize_tool_arguments("f", r#"{"path":"/tmp"#), "{}");
        // Trailing junk
        assert_eq!(sanitize_tool_arguments("f", r#"{"x":1}garbage"#), "{}");
        // Truncated (the observed failure case from the field)
        assert_eq!(sanitize_tool_arguments("f", ""), "{}");
    }

    #[test]
    fn streaming_api_error_sanitizes_and_bounds_upstream_body() {
        let secret = "sk-test-streaming-secret";
        let body = format!(r#"{{"error":"{secret} {}"}}"#, "x".repeat(4_000));
        let error = streaming_api_error(reqwest::StatusCode::UNAUTHORIZED, &body).to_string();

        assert!(error.starts_with("ModelProvider error: 401 Unauthorized:"));
        assert!(error.contains("[REDACTED]"));
        assert!(!error.contains(secret));
        assert!(error.chars().count() <= 550);
    }

    #[test]
    fn streaming_api_error_extracts_message_from_stringified_error_envelope() {
        let message =
            "anthropic error: Message: fetch failed Cause: AggregateError Name: TypeError";
        let body = serde_json::json!({
            "status": "failure",
            "message": message,
            "error": serde_json::json!({
                "message": message,
                "type": "APIError",
                "code": "500",
            })
            .to_string(),
            "error_origin_level": "api_error",
            "provider": "anthropic",
        })
        .to_string();

        let error =
            streaming_api_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, &body).to_string();

        assert_eq!(
            error,
            format!("ModelProvider error: 500 Internal Server Error: {message}")
        );
    }

    #[tokio::test]
    async fn absent_catalog_sources_return_typed_unsupported_for_ids_and_pricing() {
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("No catalog")
            .base_url("http://127.0.0.1:9")
            .auth_style(AuthStyle::Bearer)
            .build();

        let ids_error = provider
            .list_models()
            .await
            .expect_err("missing live and static ID catalogs must be typed unsupported");
        assert!(
            ids_error
                .downcast_ref::<zeroclaw_api::model_provider::ModelListingUnsupportedError>()
                .is_some()
        );

        let pricing_error = provider
            .list_models_with_pricing()
            .await
            .expect_err("missing live and static pricing catalogs must be typed unsupported");
        assert!(
            pricing_error
                .downcast_ref::<zeroclaw_api::model_provider::ModelListingUnsupportedError>()
                .is_some()
        );
    }

    fn make_model_provider(
        name: &str,
        url: &str,
        key: Option<&str>,
    ) -> OpenAiCompatibleModelProvider {
        OpenAiCompatibleModelProvider::builder("test")
            .display_name(name)
            .base_url(url)
            .credential(key)
            .auth_style(AuthStyle::Bearer)
            .build()
    }

    async fn mock_non_streaming_response(
        response_body: serde_json::Value,
    ) -> (
        OpenAiCompatibleModelProvider,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use axum::{Json, Router, routing::post};
        use tokio::net::TcpListener;

        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_for_route = std::sync::Arc::clone(&captured);
        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(request): Json<serde_json::Value>| {
                let captured = std::sync::Arc::clone(&captured_for_route);
                let response_body = response_body.clone();
                async move {
                    captured.lock().unwrap().push(request);
                    Json(response_body)
                }
            }),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let provider = make_model_provider("custom", &format!("http://{addr}"), Some("test-key"));

        (provider, captured, server)
    }

    fn make_cache_passthrough_model_provider(
        name: &str,
        url: &str,
    ) -> OpenAiCompatibleModelProvider {
        OpenAiCompatibleModelProvider::builder("test")
            .display_name(name)
            .base_url(url)
            .auth_style(AuthStyle::Bearer)
            .with_cache_passthrough()
            .build()
    }

    /// Capability pin: the compat provider reports prompt caching exactly
    /// when the flag is set, so routing and UI gating can trust capability
    /// reporting instead of probing the wire.
    #[test]
    fn cache_passthrough_capability_reports_flag_state() {
        let off = make_model_provider("custom", "http://127.0.0.1:1", None);
        assert!(!off.capabilities().prompt_caching);
        let on = make_cache_passthrough_model_provider("custom", "http://127.0.0.1:1");
        assert!(on.capabilities().prompt_caching);
    }

    /// Byte-identity pin for the default path: the flag-off request body is
    /// fully specified, so any future injection work that leaks into the
    /// default path fails this test instead of silently changing the wire.
    #[tokio::test]
    async fn cache_passthrough_flag_off_request_body_is_pinned() {
        let (provider, captured, server) = mock_non_streaming_response(serde_json::json!({
            "choices": [{"message": {"content": "ok"}}]
        }))
        .await;
        let result = provider
            .chat_with_system(Some("be brief"), "hello", "test-model", None)
            .await;
        server.abort();
        let result = result.unwrap_or_else(|error| panic!("flag-off request failed: {error}"));
        assert_eq!(result, "ok");
        let requests = captured.lock().unwrap();
        assert_eq!(
            requests[0],
            serde_json::json!({
                "model": "test-model",
                "messages": [
                    {"role": "system", "content": "be brief"},
                    {"role": "user", "content": "hello"},
                ],
                "stream": false,
            }),
            "flag-off request body must stay byte-identical to the pre-feature wire"
        );
    }

    /// Capture mock for the TTL path: passthrough on or off, provider
    /// built with the requested `cache_ttl` when set. Same wire as
    /// [`Self::mock_streaming_cache_capture`] so tests pin both paths
    /// against one shape.
    async fn mock_cache_capture_with_ttl(
        cache_passthrough: bool,
        cache_ttl: Option<CacheTtl>,
    ) -> (
        OpenAiCompatibleModelProvider,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::post};
        use tokio::net::TcpListener;

        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_for_route = std::sync::Arc::clone(&captured);
        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(body): Json<serde_json::Value>| {
                let captured = std::sync::Arc::clone(&captured_for_route);
                async move {
                    let streaming =
                        body.get("stream").and_then(serde_json::Value::as_bool) == Some(true);
                    captured.lock().unwrap().push(body);
                    if streaming {
                        return (
                            StatusCode::OK,
                            [("content-type", "text/event-stream")],
                            concat!(
                                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
                                "data: [DONE]\n\n"
                            ),
                        )
                            .into_response();
                    }
                    Json(serde_json::json!({
                        "choices": [{"message": {"content": "ok"}}]
                    }))
                    .into_response()
                }
            }),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut builder = OpenAiCompatibleModelProvider::builder("test")
            .display_name("custom")
            .base_url(&format!("http://{addr}"))
            .auth_style(AuthStyle::Bearer);
        if cache_passthrough {
            builder = builder.with_cache_passthrough();
        }
        if let Some(cache_ttl) = cache_ttl {
            builder = builder.with_cache_ttl(cache_ttl);
        }
        let provider = builder.build();

        (provider, captured, server)
    }

    /// D2 + D5: behind the flag, the 1h lifetime lands on every breakpoint
    /// the compat provider places (system prompt; rolling last message),
    /// and only breakpoint-carrying messages convert to block form. With
    /// the default lifetime the body contains no `ttl` key at all.
    #[tokio::test]
    async fn cache_ttl_one_hour_marks_every_compat_breakpoint() {
        let (provider, captured, server) =
            mock_cache_capture_with_ttl(true, Some(CacheTtl::OneHour)).await;
        let messages = vec![
            ChatMessage::system("be brief"),
            ChatMessage::user("first question"),
            ChatMessage::assistant("first answer"),
            ChatMessage::user("second question"),
        ];
        let result = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test-model",
                None,
            )
            .await;
        server.abort();
        result.unwrap_or_else(|error| panic!("1h request failed: {error}"));

        {
            let requests = captured.lock().unwrap();
            let body = &requests[0];
            let msgs = body["messages"].as_array().expect("messages array");

            let system_block = msgs[0]["content"][0]["cache_control"]
                .as_object()
                .expect("system breakpoint in block form");
            assert_eq!(system_block["type"], "ephemeral");
            assert_eq!(
                system_block["ttl"], "1h",
                "system marker carries the 1h lifetime"
            );

            let rolling_block = msgs[3]["content"][0]["cache_control"]
                .as_object()
                .expect("rolling breakpoint in block form");
            assert_eq!(rolling_block["type"], "ephemeral");
            assert_eq!(
                rolling_block["ttl"], "1h",
                "rolling marker carries the 1h lifetime"
            );

            assert_eq!(
                msgs[2]["content"], "first answer",
                "non-carrier messages must keep plain string serialization"
            );
        }

        // Default-lifetime control run: same placement, no ttl anywhere.
        let (provider, captured, server) = mock_cache_capture_with_ttl(true, None).await;
        let messages = vec![
            ChatMessage::system("be brief"),
            ChatMessage::user("first question"),
            ChatMessage::assistant("first answer"),
            ChatMessage::user("second question"),
        ];
        let result = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test-model",
                None,
            )
            .await;
        server.abort();
        result.unwrap_or_else(|error| panic!("default request failed: {error}"));
        let requests = captured.lock().unwrap();
        let body = &requests[0];
        assert!(
            !body.to_string().contains("\"ttl\""),
            "default config must keep the compat wire free of ttl keys: {body}"
        );
        let msgs = body["messages"].as_array().expect("messages array");
        assert_eq!(msgs[0]["content"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(msgs[3]["content"][0]["cache_control"]["type"], "ephemeral");
    }

    /// D3: `cache_ttl` without `cache_passthrough` is inert — the structured
    /// `chat` path hits the `!cache_passthrough` early return in
    /// `apply_cache_breakpoints`, so the body is byte-identical to the
    /// flag-off structured wire and a staged value waiting for a passthrough
    /// flip changes nothing on the wire. Removing that early return fails
    /// this test (breakpoints would appear in the body).
    #[tokio::test]
    async fn cache_ttl_one_hour_without_passthrough_is_inert() {
        let (provider, captured, server) =
            mock_cache_capture_with_ttl(false, Some(CacheTtl::OneHour)).await;
        let messages = vec![
            ChatMessage::system("be brief"),
            ChatMessage::user("first question"),
            ChatMessage::assistant("first answer"),
            ChatMessage::user("second question"),
        ];
        let result = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test-model",
                None,
            )
            .await;
        server.abort();
        result.unwrap_or_else(|error| panic!("inert request failed: {error}"));

        let requests = captured.lock().unwrap();
        assert_eq!(
            requests[0],
            serde_json::json!({
                "model": "test-model",
                "messages": [
                    {"role": "system", "content": "be brief"},
                    {"role": "user", "content": "first question"},
                    {"role": "assistant", "content": "first answer"},
                    {"role": "user", "content": "second question"},
                ],
                "stream": false,
            }),
            "cache_ttl without passthrough must leave the structured body identical to flag-off"
        );
    }

    /// Streaming capture mock: records every request body, answers with a
    /// minimal SSE stream. Returns the provider built with or without the
    /// cache flag so tests can pin both wire paths.
    async fn mock_streaming_cache_capture(
        cache_passthrough: bool,
    ) -> (
        OpenAiCompatibleModelProvider,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::post};
        use tokio::net::TcpListener;

        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_for_route = std::sync::Arc::clone(&captured);
        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(body): Json<serde_json::Value>| {
                let captured = std::sync::Arc::clone(&captured_for_route);
                async move {
                    let streaming =
                        body.get("stream").and_then(serde_json::Value::as_bool) == Some(true);
                    captured.lock().unwrap().push(body);
                    if streaming {
                        return (
                            StatusCode::OK,
                            [("content-type", "text/event-stream")],
                            concat!(
                                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
                                "data: [DONE]\n\n"
                            ),
                        )
                            .into_response();
                    }
                    Json(serde_json::json!({
                        "choices": [{"message": {"content": "ok"}}]
                    }))
                    .into_response()
                }
            }),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut builder = OpenAiCompatibleModelProvider::builder("test")
            .display_name("custom")
            .base_url(&format!("http://{addr}"))
            .auth_style(AuthStyle::Bearer);
        if cache_passthrough {
            builder = builder.with_cache_passthrough();
        }
        let provider = builder.build();

        (provider, captured, server)
    }

    /// D3 wire shape: behind the flag, the system prompt converts from
    /// string content to the single-text-block form carrying
    /// `cache_control`, while every other message keeps its existing
    /// serialization untouched.
    #[tokio::test]
    async fn cache_passthrough_system_breakpoint_serializes_block_form() {
        let (provider, captured, server) = mock_streaming_cache_capture(true).await;
        let messages = vec![ChatMessage::system("be brief"), ChatMessage::user("hello")];
        let result = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test-model",
                None,
            )
            .await;
        server.abort();
        result.unwrap_or_else(|error| panic!("flag-on request failed: {error}"));
        let requests = captured.lock().unwrap();
        assert_eq!(
            requests[0],
            serde_json::json!({
                "model": "test-model",
                "messages": [
                    {"role": "system", "content": [
                        {"type": "text", "text": "be brief",
                         "cache_control": {"type": "ephemeral"}},
                    ]},
                    {"role": "user", "content": "hello"},
                ],
                "stream": false,
            }),
            "system prompt must serialize as a cache_control text block; other messages untouched"
        );
        assert_eq!(
            requests[0].to_string().matches("cache_control").count(),
            1,
            "single exchange must carry exactly one breakpoint"
        );
    }

    /// Rolling breakpoint: once the conversation has more than one
    /// non-system message (the native provider's gate), the last message
    /// gains the second breakpoint and converts to block form; messages in
    /// between serialize exactly as before.
    #[tokio::test]
    async fn cache_passthrough_rolling_breakpoint_after_first_exchange() {
        let (provider, captured, server) = mock_streaming_cache_capture(true).await;
        let messages = vec![
            ChatMessage::system("you are brief"),
            ChatMessage::user("hi"),
            ChatMessage::assistant("hello"),
            ChatMessage::user("bye"),
        ];
        let result = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test-model",
                None,
            )
            .await;
        server.abort();
        result.unwrap_or_else(|error| panic!("flag-on history request failed: {error}"));
        let requests = captured.lock().unwrap();
        assert_eq!(
            requests[0]["messages"],
            serde_json::json!([
                {"role": "system", "content": [
                    {"type": "text", "text": "you are brief",
                     "cache_control": {"type": "ephemeral"}},
                ]},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"},
                {"role": "user", "content": [
                    {"type": "text", "text": "bye",
                     "cache_control": {"type": "ephemeral"}},
                ]},
            ]),
            "system and last message carry the breakpoints; middle messages untouched"
        );
        assert_eq!(
            requests[0].to_string().matches("cache_control").count(),
            2,
            "rolling breakpoint must never exceed two per request"
        );
    }

    /// Writes a minimal real PNG (a 1x1 transparent pixel) into a fresh
    /// tempdir and returns the dir plus the file path. The dir must outlive
    /// the request so the multimodal prepare pass can inline the file, and
    /// tests build the image marker at runtime from the path so this
    /// source never carries a literal marker.
    fn write_minimal_png() -> (tempfile::TempDir, std::path::PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let png: [u8; 67] = [
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        let image_path = temp.path().join("pixel.png");
        std::fs::write(&image_path, png).unwrap();
        (temp, image_path)
    }

    /// Wire form of [`write_minimal_png`]'s file once the multimodal
    /// prepare pass inlines it: MIME detected from the PNG signature,
    /// standard padded base64.
    const MINIMAL_PNG_DATA_URI: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAACklEQVR4nGMAAQAABQABDQottAAAAABJRU5ErkJggg==";

    /// Rolling breakpoint on an image-ending turn: the last user message
    /// serializes as [text, image] parts, and the breakpoint must land on
    /// that text part instead of vanishing because the final part is an
    /// image. The image part stays unmarked; the following turn's rolling
    /// breakpoint covers it.
    #[tokio::test]
    async fn cache_passthrough_rolling_breakpoint_lands_on_text_before_trailing_image() {
        let (provider, captured, server) = mock_streaming_cache_capture(true).await;
        let (_temp, image_path) = write_minimal_png();
        let messages = vec![
            ChatMessage::system("you are brief"),
            ChatMessage::user("hi"),
            ChatMessage::assistant("hello"),
            ChatMessage::user(format!(
                "look at this [{}:{}]",
                "IMAGE",
                image_path.display()
            )),
        ];
        let result = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test-model",
                None,
            )
            .await;
        server.abort();
        result.unwrap_or_else(|error| panic!("flag-on image-turn request failed: {error}"));
        let requests = captured.lock().unwrap();
        assert_eq!(
            requests[0]["messages"],
            serde_json::json!([
                {"role": "system", "content": [
                    {"type": "text", "text": "you are brief",
                     "cache_control": {"type": "ephemeral"}},
                ]},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"},
                {"role": "user", "content": [
                    {"type": "text", "text": "look at this",
                     "cache_control": {"type": "ephemeral"}},
                    {"type": "image_url", "image_url": {"url": MINIMAL_PNG_DATA_URI}},
                ]},
            ]),
            "system and the text part ahead of the trailing image carry the breakpoints; middle messages untouched"
        );
        assert_eq!(
            requests[0].to_string().matches("cache_control").count(),
            2,
            "an image-ending turn must still carry exactly two breakpoints"
        );
    }

    /// Streaming twin of the image-turn pin: the streaming path must place
    /// the rolling breakpoint on the same text part when the final user
    /// message ends with an image.
    #[tokio::test]
    async fn cache_passthrough_streaming_rolling_breakpoint_lands_on_text_before_trailing_image() {
        use futures_util::StreamExt as _;

        let (provider, captured, server) = mock_streaming_cache_capture(true).await;
        let (_temp, image_path) = write_minimal_png();
        let messages = vec![
            ChatMessage::system("you are brief"),
            ChatMessage::user("hi"),
            ChatMessage::assistant("hello"),
            ChatMessage::user(format!(
                "look at this [{}:{}]",
                "IMAGE",
                image_path.display()
            )),
        ];
        let events = provider
            .stream_chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test-model",
                None,
                StreamOptions {
                    enabled: true,
                    count_tokens: false,
                },
            )
            .collect::<Vec<_>>()
            .await;
        server.abort();
        assert!(
            events.iter().all(Result::is_ok),
            "streaming image-turn request must succeed: {events:?}"
        );
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]["messages"],
            serde_json::json!([
                {"role": "system", "content": [
                    {"type": "text", "text": "you are brief",
                     "cache_control": {"type": "ephemeral"}},
                ]},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"},
                {"role": "user", "content": [
                    {"type": "text", "text": "look at this",
                     "cache_control": {"type": "ephemeral"}},
                    {"type": "image_url", "image_url": {"url": MINIMAL_PNG_DATA_URI}},
                ]},
            ]),
            "streaming path must inject the image-turn breakpoints identically"
        );
        assert_eq!(
            requests[0].to_string().matches("cache_control").count(),
            2,
            "streaming image-ending turn must carry exactly two breakpoints"
        );
    }

    /// Image-only turn: a message whose content is just the image carries
    /// no text part at all, so it cannot host the rolling breakpoint. The
    /// breakpoint rolls back onto the nearest earlier non-system message
    /// with text, the image part stays unmarked, and the two-breakpoint
    /// ceiling holds.
    #[tokio::test]
    async fn cache_passthrough_rolling_breakpoint_falls_back_when_last_message_is_image_only() {
        let (provider, captured, server) = mock_streaming_cache_capture(true).await;
        let (_temp, image_path) = write_minimal_png();
        let messages = vec![
            ChatMessage::system("you are brief"),
            ChatMessage::user("hi"),
            ChatMessage::assistant("hello"),
            ChatMessage::user(format!("[{}:{}]", "IMAGE", image_path.display())),
        ];
        let result = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test-model",
                None,
            )
            .await;
        server.abort();
        result.unwrap_or_else(|error| panic!("flag-on image-only request failed: {error}"));
        let requests = captured.lock().unwrap();
        assert_eq!(
            requests[0]["messages"],
            serde_json::json!([
                {"role": "system", "content": [
                    {"type": "text", "text": "you are brief",
                     "cache_control": {"type": "ephemeral"}},
                ]},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "hello",
                     "cache_control": {"type": "ephemeral"}},
                ]},
                {"role": "user", "content": [
                    {"type": "image_url", "image_url": {"url": MINIMAL_PNG_DATA_URI}},
                ]},
            ]),
            "image-only turn rolls the breakpoint onto the nearest earlier non-system message; the image part stays unmarked"
        );
        assert_eq!(
            requests[0].to_string().matches("cache_control").count(),
            2,
            "image-only fallback must still carry exactly two breakpoints"
        );
    }

    /// Unit pin for the fallback walk on hand-built messages, so the
    /// invariant holds regardless of how a history was constructed: an
    /// image-only last message rolls the rolling breakpoint back onto the
    /// nearest earlier non-system message with text, a trailing system
    /// message never absorbs the slot or gains a second mark, and the
    /// two-breakpoint ceiling holds.
    #[test]
    fn cache_passthrough_breakpoint_walk_handles_hand_built_image_only_messages() {
        let provider = make_cache_passthrough_model_provider("custom", "http://127.0.0.1:1");
        let mut messages = vec![
            Message {
                role: "system".to_string(),
                content: MessageContent::Text("you are brief".to_string()),
                thinking_blocks: None,
            },
            Message {
                role: "user".to_string(),
                content: MessageContent::Text("hi".to_string()),
                thinking_blocks: None,
            },
            Message {
                role: "assistant".to_string(),
                content: MessageContent::Text("hello".to_string()),
                thinking_blocks: None,
            },
            Message {
                role: "user".to_string(),
                content: MessageContent::Parts(vec![MessagePart::ImageUrl {
                    image_url: ImageUrlPart {
                        url: "data:image/png;base64,abcd".to_string(),
                    },
                }]),
                thinking_blocks: None,
            },
            Message {
                role: "system".to_string(),
                content: MessageContent::Text("extra".to_string()),
                thinking_blocks: None,
            },
        ];
        provider.apply_cache_breakpoints(&mut messages, None);
        let value = serde_json::to_value(&messages).unwrap();
        assert_eq!(
            value,
            serde_json::json!([
                {"role": "system", "content": [
                    {"type": "text", "text": "you are brief",
                     "cache_control": {"type": "ephemeral"}},
                ]},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "hello",
                     "cache_control": {"type": "ephemeral"}},
                ]},
                {"role": "user", "content": [
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,abcd"}},
                ]},
                {"role": "system", "content": "extra"},
            ]),
            "image-only turn rolls the breakpoint onto the nearest earlier non-system message; system messages never take the rolling slot"
        );
        assert_eq!(
            value.to_string().matches("cache_control").count(),
            2,
            "hand-built fallback must keep the two-breakpoint ceiling"
        );
    }

    /// Flag-off twin for the multimodal shape: with the flag off, the
    /// image-ending history serializes byte-identically to the pre-feature
    /// wire, with plain string content where the flag-on path would mark,
    /// the same unmarked image parts, and no cache markers anywhere in the
    /// body.
    #[tokio::test]
    async fn cache_passthrough_flag_off_image_turn_body_unmarked() {
        let (provider, captured, server) = mock_streaming_cache_capture(false).await;
        let (_temp, image_path) = write_minimal_png();
        let messages = vec![
            ChatMessage::system("you are brief"),
            ChatMessage::user("hi"),
            ChatMessage::assistant("hello"),
            ChatMessage::user(format!(
                "look at this [{}:{}]",
                "IMAGE",
                image_path.display()
            )),
        ];
        let result = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test-model",
                None,
            )
            .await;
        server.abort();
        result.unwrap_or_else(|error| panic!("flag-off image-turn request failed: {error}"));
        let requests = captured.lock().unwrap();
        assert_eq!(
            requests[0]["messages"],
            serde_json::json!([
                {"role": "system", "content": "you are brief"},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"},
                {"role": "user", "content": [
                    {"type": "text", "text": "look at this"},
                    {"type": "image_url", "image_url": {"url": MINIMAL_PNG_DATA_URI}},
                ]},
            ]),
            "flag-off multimodal body must stay byte-identical: plain strings and unmarked image parts"
        );
        assert!(
            !requests[0].to_string().contains("cache_control"),
            "flag-off image-ending turn must carry no cache markers"
        );
    }

    /// The rolling breakpoint follows the native provider's gate: a
    /// conversation with only one non-system message gets the system
    /// breakpoint alone.
    #[tokio::test]
    async fn cache_passthrough_single_exchange_skips_rolling_breakpoint() {
        let (provider, captured, server) = mock_streaming_cache_capture(true).await;
        let messages = vec![
            ChatMessage::system("you are brief"),
            ChatMessage::user("hi"),
        ];
        let result = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test-model",
                None,
            )
            .await;
        server.abort();
        result.unwrap_or_else(|error| panic!("flag-on history request failed: {error}"));
        let requests = captured.lock().unwrap();
        assert_eq!(
            requests[0].to_string().matches("cache_control").count(),
            1,
            "one non-system message means no rolling breakpoint"
        );
        assert_eq!(
            requests[0]["messages"][1]["content"],
            serde_json::json!("hi"),
            "first user message must stay a plain string"
        );
    }

    /// Streaming twin of the system-breakpoint pin: the injection must be
    /// identical on the streaming path (a previous feature shipped the
    /// non-streaming path and silently missed streaming).
    #[tokio::test]
    async fn cache_passthrough_streaming_system_breakpoint_matches_non_streaming() {
        use futures_util::StreamExt as _;

        let (provider, captured, server) = mock_streaming_cache_capture(true).await;
        let events = provider
            .stream_chat(
                ProviderChatRequest {
                    messages: &[ChatMessage::system("be brief"), ChatMessage::user("hello")],
                    tools: None,
                    thinking: None,
                },
                "test-model",
                None,
                StreamOptions {
                    enabled: true,
                    count_tokens: false,
                },
            )
            .collect::<Vec<_>>()
            .await;
        server.abort();
        assert!(
            events.iter().all(Result::is_ok),
            "streaming request must succeed: {events:?}"
        );
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]["messages"][0],
            serde_json::json!({
                "role": "system",
                "content": [
                    {"type": "text", "text": "be brief",
                     "cache_control": {"type": "ephemeral"}},
                ],
            }),
            "streaming path must inject the system breakpoint identically"
        );
        assert_eq!(
            requests[0]["messages"][1]["content"],
            serde_json::json!("hello"),
            "streaming user message must stay a plain string"
        );
    }

    /// Flag-off streaming twin: the default path stays byte-identical on
    /// the streaming wire too.
    #[tokio::test]
    async fn cache_passthrough_flag_off_streaming_body_unmarked() {
        use futures_util::StreamExt as _;

        let (provider, captured, server) = mock_streaming_cache_capture(false).await;
        let events = provider
            .stream_chat(
                ProviderChatRequest {
                    messages: &[ChatMessage::system("be brief"), ChatMessage::user("hello")],
                    tools: None,
                    thinking: None,
                },
                "test-model",
                None,
                StreamOptions {
                    enabled: true,
                    count_tokens: false,
                },
            )
            .collect::<Vec<_>>()
            .await;
        server.abort();
        assert!(events.iter().all(Result::is_ok));
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]["messages"][0]["content"],
            serde_json::json!("be brief"),
            "flag-off system message must stay a plain string"
        );
        assert!(
            !requests[0].to_string().contains("cache_control"),
            "flag-off streaming body must carry no cache markers"
        );
    }

    /// Native-tools path: the same two-breakpoint placement applies to the
    /// native-tools request builder, so tool-bearing agent loops get the
    /// same caching shape as plain conversations.
    #[tokio::test]
    async fn cache_passthrough_tools_path_rolls_breakpoint_onto_last_message() {
        let (provider, captured, server) = mock_streaming_cache_capture(true).await;
        let messages = vec![
            ChatMessage::system("you are brief"),
            ChatMessage::user("hi"),
            ChatMessage::assistant("hello"),
            ChatMessage::user("bye"),
        ];
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get weather",
                "parameters": {"type": "object", "properties": {}}
            }
        })];
        let result = provider
            .chat_with_tools(&messages, &tools, "test-model", None)
            .await;
        server.abort();
        result.unwrap_or_else(|error| panic!("flag-on tools request failed: {error}"));
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].to_string().matches("cache_control").count(),
            2,
            "tools path must carry at most the two standard breakpoints"
        );
        assert_eq!(
            requests[0]["messages"][0]["content"][0]["cache_control"],
            serde_json::json!({"type": "ephemeral"}),
            "system breakpoint present on the tools path"
        );
        assert_eq!(
            requests[0]["messages"].as_array().unwrap().last().unwrap()["content"][0]["cache_control"],
            serde_json::json!({"type": "ephemeral"}),
            "rolling breakpoint lands on the last message of the tools path"
        );
    }

    fn non_streaming_response_cases() -> Vec<(&'static str, serde_json::Value, Option<&'static str>)>
    {
        vec![
            (
                "direct",
                serde_json::json!({
                    "choices": [{"message": {"content": "direct"}}]
                }),
                Some("direct"),
            ),
            (
                "wrapped",
                serde_json::json!({
                    "data": {
                        "choices": [{"message": {"content": "wrapped"}}]
                    },
                    "success": true
                }),
                Some("wrapped"),
            ),
            (
                "direct_precedence",
                serde_json::json!({
                    "choices": [{"message": {"content": "top-level"}}],
                    "data": {
                        "choices": [{"message": {"content": "nested"}}]
                    }
                }),
                Some("top-level"),
            ),
            (
                "malformed",
                serde_json::json!({
                    "data": {
                        "choices": "invalid",
                        "api_key": "sk-test-secret-value"
                    }
                }),
                None,
            ),
        ]
    }

    fn assert_sanitized_envelope_error(error: &anyhow::Error, case: &str) {
        let message = error.to_string();
        assert!(
            message.contains("custom API returned an unexpected chat-completions payload"),
            "{case}: unexpected error: {message}"
        );
        assert!(
            message.contains("[REDACTED]"),
            "{case}: sanitized body missing redaction: {message}"
        );
        assert!(
            !message.contains("sk-test-secret-value"),
            "{case}: secret leaked in error: {message}"
        );
    }

    #[test]
    fn convert_tool_specs_serializes_openai_wire_shape() {
        let p = make_model_provider("vllm", "http://localhost:8000/v1", None);
        // Clean schema (shared as-is) and dirty schema (rewritten by the
        // OpenAI strategy's strategy-independent passes).
        let tools = vec![
            zeroclaw_api::tool::ToolSpec::new(
                "get_weather",
                "Fetch the weather",
                serde_json::json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } }
                }),
            ),
            zeroclaw_api::tool::ToolSpec::new(
                "set_mode",
                "Set the mode",
                serde_json::json!({
                    "type": "object",
                    "properties": { "mode": { "const": "fast" } }
                }),
            ),
        ];

        let converted = p.convert_tool_specs(Some(&tools)).expect("Some(tools) in");
        let raw = serde_json::to_string(&converted).unwrap();

        // Raw string, not `serde_json::Value` equality: `Value` object
        // equality ignores key order, so it cannot pin the declared
        // key-order delta (typed structs serialize `type`/`function`, and
        // `name`/`description`/`parameters` within it, in field-declaration
        // order; the `parameters` schema itself is a plain `Value` with no
        // `preserve_order` feature enabled, so its own keys always come out
        // alphabetical regardless of insertion order, e.g. `properties`
        // before `type`). `const` is also rewritten to a single-value
        // `enum` by the cleaner, exactly as the pre-typed-struct pipeline
        // did.
        assert_eq!(
            raw,
            concat!(
                r#"[{"type":"function","function":{"name":"get_weather","description":"Fetch the weather","parameters":{"properties":{"city":{"type":"string"}},"type":"object"}}},"#,
                r#"{"type":"function","function":{"name":"set_mode","description":"Set the mode","parameters":{"properties":{"mode":{"enum":["fast"]}},"type":"object"}}}]"#
            ),
            "typed tool specs must serialize to the same byte-for-byte wire \
             shape (including key order) as the previous json!-built payload"
        );
    }

    #[test]
    fn convert_tool_specs_shares_clean_schema_and_memoizes_dirty_schema() {
        let p = make_model_provider("vllm", "http://localhost:8000/v1", None);
        let tools = vec![
            zeroclaw_api::tool::ToolSpec::new(
                "clean_tool",
                "already clean",
                serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } }
                }),
            ),
            zeroclaw_api::tool::ToolSpec::new(
                "dirty_tool",
                "needs cleaning",
                serde_json::json!({ "type": "string", "const": "x" }),
            ),
        ];

        let first = p.convert_tool_specs(Some(&tools)).unwrap();
        let second = p.convert_tool_specs(Some(&tools)).unwrap();

        assert!(
            std::sync::Arc::ptr_eq(&first[0].function.parameters, &tools[0].parameters),
            "clean schemas must be shared straight from the registry Arc"
        );
        assert!(
            std::sync::Arc::ptr_eq(
                &first[1].function.parameters,
                &second[1].function.parameters
            ),
            "dirty schemas must be cleaned once and memoized, not re-copied per request"
        );
    }

    #[test]
    fn streaming_native_tool_request_serializes_tools_and_guards_tool_choice() {
        let p = make_model_provider("vllm", "http://localhost:8000/v1", None);
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![zeroclaw_api::tool::ToolSpec::new(
            "get_weather",
            "Fetch the weather",
            serde_json::json!({ "type": "object", "properties": {} }),
        )];
        let converted = p.convert_tool_specs_for_model(Some(&tools), "test-model");

        let value = serde_json::to_value(p.build_streaming_native_tool_request(
            "test-model",
            &messages,
            converted,
            Some(0.5),
            true,
            false,
            false,
            None,
        ))
        .unwrap();

        assert_eq!(value["stream"], serde_json::json!(true));
        assert_eq!(
            value["stream_options"]["include_usage"],
            serde_json::json!(true)
        );
        assert_eq!(value["tool_choice"], serde_json::json!("auto"));
        assert_eq!(
            value["tools"],
            serde_json::json!([{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Fetch the weather",
                    "parameters": { "type": "object", "properties": {} }
                }
            }]),
            "streaming payload must carry the typed tools in OpenAI wire shape"
        );

        // Converted-empty tools must omit tool_choice (vLLM 0.19+ rejects
        // tool_choice without a tools field).
        let empty = serde_json::to_value(p.build_streaming_native_tool_request(
            "test-model",
            &messages,
            Some(vec![]),
            None,
            true,
            false,
            false,
            None,
        ))
        .unwrap();
        assert!(
            empty.get("tool_choice").is_none(),
            "empty converted tools must not set tool_choice; got: {empty}"
        );
    }

    #[test]
    fn provider_clones_share_one_schema_memo() {
        // stream_chat clones the provider per call and relies on the
        // Arc<SchemaCleanCache> field so the clone shares the instance memo;
        // a rebuild-per-call refactor would silently reintroduce per-request
        // cold-cache cleaning on the streaming path with identical wire
        // bytes, so pin the sharing directly.
        let p = make_model_provider("vllm", "http://localhost:8000/v1", None);
        let tools = vec![zeroclaw_api::tool::ToolSpec::new(
            "dirty_tool",
            "needs cleaning",
            serde_json::json!({ "type": "string", "const": "x" }),
        )];

        let original = p.convert_tool_specs(Some(&tools)).unwrap();
        let via_clone = p.clone().convert_tool_specs(Some(&tools)).unwrap();

        assert!(
            std::sync::Arc::ptr_eq(
                &original[0].function.parameters,
                &via_clone[0].function.parameters
            ),
            "provider clones must serve dirty schemas from the same memo"
        );
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

    // Regression: vLLM 0.19+ and spec-compliant validators reject
    // `tool_choice` when `tools` is absent or empty (HTTP 400:
    // "When using `tool_choice`, `tools` must be set."). The request builders
    // must omit `tool_choice` whenever the converted tool list is empty.
    #[test]
    fn thinking_passthrough_flag_off_never_injects_thinking() {
        // Flag off = today's behavior exactly, regardless of runtime params.
        // Byte-identical guarantee, asserted two ways: the request payload
        // carries no `thinking` key (and is identical with params present or
        // absent), and the extra_body seam returns the configured extra_body
        // value unchanged.
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };
        let messages = vec![ChatMessage::user("hello")];

        let p = make_model_provider("gateway", "http://localhost:8000/v1", None);
        let with_params = serde_json::to_value(p.build_native_tool_chat_request(
            &messages,
            None,
            "test-model",
            None,
            false,
            false,
            Some(params),
        ))
        .unwrap();
        let without_params = serde_json::to_value(p.build_native_tool_chat_request(
            &messages,
            None,
            "test-model",
            None,
            false,
            false,
            None,
        ))
        .unwrap();
        assert!(
            with_params.get("thinking").is_none(),
            "flag-off request must never gain a thinking key; got: {with_params}"
        );
        assert_eq!(
            with_params, without_params,
            "flag-off requests must be byte-identical whether params are present or not"
        );

        let extra = serde_json::json!({"top_k": 5});
        let p_with_extra = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .extra_body(extra.clone())
            .build();
        assert_eq!(
            p_with_extra.request_extra_body("test-model", Some(params)),
            Some(extra),
            "flag-off extra_body seam must return the configured extra_body unchanged"
        );
    }

    #[test]
    fn thinking_passthrough_injects_enabled_budget_shape() {
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let messages = vec![ChatMessage::user("hello")];

        let req = p.build_native_tool_chat_request(
            &messages,
            None,
            "test-model",
            None,
            false,
            false,
            Some(params),
        );
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value["thinking"],
            serde_json::json!({"type": "enabled", "budget_tokens": 8_192}),
            "passthrough must inject the Anthropic enabled-budget thinking shape"
        );

        // Streaming builder mirrors the non-streaming injection... except it
        // must NOT: passthrough never attaches thinking to the streamed wire
        // (unverified SSE frames, no signed capture on streamed responses).
        // The streamed body behaves like thinking-off for that turn.
        let streaming = serde_json::to_value(p.build_streaming_native_tool_request(
            "test-model",
            &messages,
            Some(vec![]),
            None,
            true,
            false,
            false,
            Some(params),
        ))
        .unwrap();
        assert!(
            streaming.get("thinking").is_none(),
            "streamed requests under passthrough must never carry the thinking object: {streaming}"
        );
    }

    #[test]
    fn thinking_passthrough_streaming_builder_drops_thinking_display_variants_too() {
        // Defense in depth: every display variant is equally withheld from
        // the streamed wire under passthrough.
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let messages = vec![ChatMessage::user("hello")];
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: Some(zeroclaw_api::model_provider::ThinkingDisplay::Summarized),
        };

        let with_tools = serde_json::to_value(p.build_streaming_native_tool_request(
            "test-model",
            &messages,
            Some(vec![]),
            None,
            true,
            false,
            false,
            Some(params),
        ))
        .unwrap();
        assert!(with_tools.get("thinking").is_none());

        let flag_off = make_model_provider("gateway", "http://localhost:8000/v1", None);
        let legacy = serde_json::to_value(flag_off.build_streaming_native_tool_request(
            "test-model",
            &messages,
            Some(vec![]),
            None,
            true,
            false,
            false,
            Some(params),
        ))
        .unwrap();
        assert!(
            legacy.get("thinking").is_none(),
            "flag-off streaming never injected thinking and must stay that way"
        );
    }

    #[test]
    fn thinking_passthrough_adaptive_model_sends_adaptive_shape_without_budget() {
        // Adaptive-only models reject the fixed-budget shape with HTTP 400;
        // the injected object must switch to the bare adaptive shape.
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();

        let resolved = p
            .request_extra_body("claude-opus-4-7", Some(params))
            .unwrap();
        assert_eq!(
            resolved["thinking"],
            serde_json::json!({"type": "adaptive"}),
            "adaptive-only models must receive the adaptive shape"
        );
        assert!(
            resolved["thinking"].get("budget_tokens").is_none(),
            "adaptive shape must not carry a budget_tokens key"
        );
    }

    #[test]
    fn thinking_passthrough_budget_model_shape_unchanged_by_style_share() {
        // A budget model keeps the enabled shape exactly, with no display
        // key when params.display is None.
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();

        let resolved = p.request_extra_body("test-model", Some(params)).unwrap();
        assert_eq!(
            resolved["thinking"],
            serde_json::json!({"type": "enabled", "budget_tokens": 8_192}),
            "budget models must keep the enabled shape; display None must omit the key"
        );
    }

    #[test]
    fn thinking_passthrough_display_includes_snake_case_value_on_both_styles() {
        // params.display rides on both shapes, serialized snake_case like
        // the native provider's NativeThinkingConfig.
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();

        let updates = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: Some(zeroclaw_api::model_provider::ThinkingDisplay::Updates),
        };
        let budget = p.request_extra_body("test-model", Some(updates)).unwrap();
        assert_eq!(
            budget["thinking"],
            serde_json::json!({
                "type": "enabled",
                "budget_tokens": 8_192,
                "display": "updates"
            }),
            "display Some must emit the snake_case value on the budget shape"
        );

        let adaptive = p
            .request_extra_body("claude-opus-4-7", Some(updates))
            .unwrap();
        assert_eq!(
            adaptive["thinking"],
            serde_json::json!({"type": "adaptive", "display": "updates"}),
            "display Some must emit the snake_case value on the adaptive shape"
        );

        let omitted = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: Some(zeroclaw_api::model_provider::ThinkingDisplay::Omitted),
        };
        let omitted_body = p.request_extra_body("test-model", Some(omitted)).unwrap();
        assert_eq!(
            omitted_body["thinking"]["display"],
            serde_json::json!("omitted"),
            "each display variant must serialize to its snake_case name"
        );
    }

    #[test]
    fn thinking_passthrough_prefixed_adaptive_model_id_resolves_adaptive() {
        // Gateway routing prefixes model IDs; the shared style resolver
        // matches on substrings, so a prefixed adaptive-only ID must still
        // resolve to the adaptive shape (live gateways emit IDs like
        // "claude-group/claude-fable-5").
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();

        for model in [
            "claude-group/claude-opus-4-7",
            "team-alias/claude-fable-5-1",
        ] {
            let resolved = p.request_extra_body(model, Some(params)).unwrap();
            assert_eq!(
                resolved["thinking"],
                serde_json::json!({"type": "adaptive"}),
                "prefixed adaptive-only ID {model} must resolve to the adaptive shape"
            );
        }
    }

    #[test]
    fn thinking_passthrough_adaptive_shape_flows_through_request_builder() {
        // End to end through the non-streaming builder: an adaptive-only
        // gateway model gets the adaptive thinking object at the top level.
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: Some(zeroclaw_api::model_provider::ThinkingDisplay::Summarized),
        };
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let messages = vec![ChatMessage::user("hello")];

        let req = p.build_native_tool_chat_request(
            &messages,
            None,
            "claude-group/claude-fable-5-1",
            None,
            false,
            false,
            Some(params),
        );
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value["thinking"],
            serde_json::json!({"type": "adaptive", "display": "summarized"}),
            "the request builder must carry the adaptive plus display shape to the wire"
        );
    }

    #[test]
    fn thinking_passthrough_without_params_sends_no_thinking() {
        // Flag on but the runtime supplied no native thinking params: nothing
        // to forward, request must stay free of a thinking key.
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let messages = vec![ChatMessage::user("hello")];

        let req = p.build_native_tool_chat_request(
            &messages,
            None,
            "test-model",
            None,
            false,
            false,
            None,
        );
        let value = serde_json::to_value(&req).unwrap();
        assert!(
            value.get("thinking").is_none(),
            "params-None request must not carry a thinking key; got: {value}"
        );
        assert!(p.request_extra_body("test-model", None).is_none());
    }

    #[test]
    fn thinking_passthrough_extra_body_keys_win_over_injected_thinking() {
        // Explicit operator `extra_body` always wins: an extra_body `thinking`
        // key must fully shadow the injected object.
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .extra_body(serde_json::json!({"thinking": {"type": "off"}}))
            .build();

        let resolved = p.request_extra_body("test-model", Some(params)).unwrap();
        assert_eq!(
            resolved["thinking"],
            serde_json::json!({"type": "off"}),
            "explicit extra_body thinking key must win over the injected object"
        );
        assert!(
            resolved.get("budget_tokens").is_none(),
            "injected budget_tokens must not leak to the top level"
        );
    }

    #[tokio::test]
    async fn thinking_passthrough_explicit_enabled_override_raises_limit_above_its_budget() {
        // An explicit `provider_extra` thinking key wins over the injected
        // object, so normalization must follow the object that is actually
        // serialized: an `enabled` override with its own budget raises
        // `max_tokens` above THAT budget (the runtime budget is not the one
        // on the wire) and still forces temperature 1.0.
        use axum::Json;
        use axum::Router;
        use axum::routing::post;
        use std::sync::{Arc, Mutex};
        use tokio::net::TcpListener;

        let bodies = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let bodies_for_route = Arc::clone(&bodies);
        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(body): Json<serde_json::Value>| {
                let bodies = Arc::clone(&bodies_for_route);
                async move {
                    bodies.lock().unwrap().push(body);
                    Json(serde_json::json!({
                        "choices": [{"message": {"content": "ok"}}]
                    }))
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{addr}");
        let messages = vec![ChatMessage::user("hello")];
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };

        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url(&base_url)
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .extra_body(serde_json::json!({
                "thinking": {"type": "enabled", "budget_tokens": 16_384}
            }))
            .max_tokens(Some(10_000))
            .build();
        p.chat_with_history_inner(&messages, "test-model", Some(0.7), Some(params))
            .await
            .unwrap();

        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1, "one captured body per request");
        assert_eq!(
            bodies[0]["thinking"]["budget_tokens"],
            serde_json::json!(16_384),
            "explicit extra_body thinking budget must win over the injected one"
        );
        assert_eq!(
            bodies[0]["max_tokens"],
            serde_json::json!(16_385),
            "max_tokens must be raised above the effective (override) budget; got max_tokens = {}",
            bodies[0]["max_tokens"]
        );
        assert_eq!(
            bodies[0]["temperature"],
            serde_json::json!(1.0),
            "an effective enabled thinking object still forces temperature 1.0"
        );
        drop(bodies);
        server.abort();
    }

    #[tokio::test]
    async fn thinking_passthrough_explicit_off_override_keeps_temperature_and_limit() {
        // The `off` override is the object actually serialized, so the
        // request is not thinking-enabled: the caller's temperature and
        // limit stand instead of the injected-shape normalization.
        use axum::Json;
        use axum::Router;
        use axum::routing::post;
        use std::sync::{Arc, Mutex};
        use tokio::net::TcpListener;

        let bodies = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let bodies_for_route = Arc::clone(&bodies);
        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(body): Json<serde_json::Value>| {
                let bodies = Arc::clone(&bodies_for_route);
                async move {
                    bodies.lock().unwrap().push(body);
                    Json(serde_json::json!({
                        "choices": [{"message": {"content": "ok"}}]
                    }))
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{addr}");
        let messages = vec![ChatMessage::user("hello")];
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };

        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url(&base_url)
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .extra_body(serde_json::json!({"thinking": {"type": "off"}}))
            .max_tokens(Some(10_000))
            .build();
        p.chat_with_history_inner(&messages, "test-model", Some(0.7), Some(params))
            .await
            .unwrap();

        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1, "one captured body per request");
        assert_eq!(
            bodies[0]["thinking"],
            serde_json::json!({"type": "off"}),
            "explicit extra_body thinking key must win over the injected object"
        );
        assert_eq!(
            bodies[0]["temperature"],
            serde_json::json!(0.7),
            "an effective off thinking object must keep the caller's temperature"
        );
        assert_eq!(
            bodies[0]["max_tokens"],
            serde_json::json!(10_000),
            "an effective off thinking object must keep the configured limit"
        );
        drop(bodies);
        server.abort();
    }

    #[test]
    fn thinking_passthrough_override_shapes_flow_through_native_tool_builder() {
        // The typed native-tools path resolves from the same effective
        // object as the shared builder: an `off` override keeps the
        // caller's values, an `enabled` override raises the limit above
        // its own budget and still forces temperature 1.0.
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };
        let messages = vec![ChatMessage::user("hello")];

        let off = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .extra_body(serde_json::json!({"thinking": {"type": "off"}}))
            .max_tokens(Some(10_000))
            .build();
        let off_body = serde_json::to_value(off.build_native_tool_chat_request(
            &messages,
            Some(vec![NativeToolSpec {
                kind: "function".to_string(),
                extra: serde_json::Map::new(),
                function: NativeToolFunctionSpec {
                    extra: serde_json::Map::new(),
                    name: "get_weather".to_string(),
                    description: String::new(),
                    parameters: std::sync::Arc::new(serde_json::json!({})),
                },
            }]),
            "test-model",
            Some(0.7),
            false,
            false,
            Some(params),
        ))
        .unwrap();
        assert_eq!(
            off_body["thinking"],
            serde_json::json!({"type": "off"}),
            "explicit off override must win on the typed path; got: {off_body}"
        );
        assert_eq!(
            off_body["temperature"],
            serde_json::json!(0.7),
            "effective off object keeps the caller's temperature on the typed path"
        );
        assert_eq!(
            off_body["max_tokens"],
            serde_json::json!(10_000),
            "effective off object keeps the configured limit on the typed path"
        );

        let enabled = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .extra_body(serde_json::json!({
                "thinking": {"type": "enabled", "budget_tokens": 16_384}
            }))
            .max_tokens(Some(10_000))
            .build();
        let enabled_body = serde_json::to_value(enabled.build_native_tool_chat_request(
            &messages,
            Some(vec![NativeToolSpec {
                kind: "function".to_string(),
                extra: serde_json::Map::new(),
                function: NativeToolFunctionSpec {
                    extra: serde_json::Map::new(),
                    name: "get_weather".to_string(),
                    description: String::new(),
                    parameters: std::sync::Arc::new(serde_json::json!({})),
                },
            }]),
            "test-model",
            Some(0.7),
            false,
            false,
            Some(params),
        ))
        .unwrap();
        assert_eq!(
            enabled_body["thinking"]["budget_tokens"],
            serde_json::json!(16_384),
            "explicit enabled override budget must win on the typed path; got: {enabled_body}"
        );
        assert_eq!(
            enabled_body["max_tokens"],
            serde_json::json!(16_385),
            "typed path must raise the limit above the effective override budget; got: {enabled_body}"
        );
        assert_eq!(
            enabled_body["temperature"],
            serde_json::json!(1.0),
            "effective enabled object still forces temperature 1.0 on the typed path"
        );
    }

    #[tokio::test]
    async fn thinking_passthrough_prompt_guided_fallback_never_injects_or_normalizes() {
        // Streaming sites pass streaming_thinking_params, which is None
        // whenever passthrough is on: the tool-less streaming fallback can
        // never inject the thinking object, so it never normalizes either.
        // Without an override the body carries no `thinking` key at all;
        // an operator's explicit override rides along as their own request
        // with the caller's temperature and limit untouched.
        use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::post};
        use futures_util::StreamExt as _;
        use std::sync::{Arc, Mutex};
        use tokio::net::TcpListener;

        let bodies = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let bodies_for_route = Arc::clone(&bodies);
        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(body): Json<serde_json::Value>| {
                let bodies = Arc::clone(&bodies_for_route);
                async move {
                    let streaming =
                        body.get("stream").and_then(serde_json::Value::as_bool) == Some(true);
                    bodies.lock().unwrap().push(body);
                    if streaming {
                        return (
                            StatusCode::OK,
                            [("content-type", "text/event-stream")],
                            concat!(
                                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
                                "data: [DONE]\n\n"
                            ),
                        )
                            .into_response();
                    }
                    Json(serde_json::json!({
                        "choices": [{"message": {"content": "ok"}}]
                    }))
                    .into_response()
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{addr}");
        let messages = vec![ChatMessage::user("hello")];
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };

        let plain = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url(&base_url)
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .max_tokens(Some(10_000))
            .build();
        let events = plain
            .stream_chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: Some(params),
                },
                "test-model",
                Some(0.75),
                StreamOptions {
                    enabled: true,
                    count_tokens: false,
                },
            )
            .collect::<Vec<_>>()
            .await;
        assert!(
            events.iter().all(Result::is_ok),
            "fallback stream must succeed: {events:?}"
        );

        let override_provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url(&base_url)
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .extra_body(serde_json::json!({
                "thinking": {"type": "enabled", "budget_tokens": 16_384}
            }))
            .max_tokens(Some(10_000))
            .build();
        let events = override_provider
            .stream_chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: Some(params),
                },
                "test-model",
                Some(0.7),
                StreamOptions {
                    enabled: true,
                    count_tokens: false,
                },
            )
            .collect::<Vec<_>>()
            .await;
        assert!(
            events.iter().all(Result::is_ok),
            "override fallback stream must succeed: {events:?}"
        );

        server.abort();
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2, "one captured body per stream");
        assert!(
            bodies[0].get("thinking").is_none(),
            "passthrough streaming fallback must not inject a thinking object; got: {}",
            bodies[0]
        );
        assert_eq!(
            bodies[0]["temperature"],
            serde_json::json!(0.75),
            "no injection means no temperature forcing on the fallback body"
        );
        assert_eq!(
            bodies[0]["max_tokens"],
            serde_json::json!(10_000),
            "no injection means no limit raising on the fallback body"
        );
        assert_eq!(
            bodies[1]["thinking"],
            serde_json::json!({"type": "enabled", "budget_tokens": 16_384}),
            "an explicit override rides the fallback body as the operator's own request"
        );
        assert_eq!(
            bodies[1]["temperature"],
            serde_json::json!(0.7),
            "the fallback never normalizes, even with an enabled override configured"
        );
        assert_eq!(
            bodies[1]["max_tokens"],
            serde_json::json!(10_000),
            "the fallback never raises the limit, even with an enabled override configured"
        );
    }

    #[test]
    fn thinking_passthrough_merges_alongside_unrelated_extra_body_keys() {
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 4_096,
            display: None,
        };
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .extra_body(serde_json::json!({"top_k": 5}))
            .build();

        let resolved = p.request_extra_body("test-model", Some(params)).unwrap();
        assert_eq!(
            resolved["thinking"],
            serde_json::json!({"type": "enabled", "budget_tokens": 4_096}),
            "injected thinking object must ride alongside unrelated extra_body keys"
        );
        assert_eq!(resolved["top_k"], serde_json::json!(5));
    }

    #[test]
    fn thinking_passthrough_non_object_extra_body_wins_outright() {
        // A non-object extra_body cannot be key-merged; the explicit config
        // value wins outright. (Serializing it would fail at the existing
        // serde(flatten) boundary — unchanged pre-existing behavior — so
        // this pins the seam, not the wire.)
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .extra_body(serde_json::json!("scalar"))
            .build();

        assert_eq!(
            p.request_extra_body("test-model", Some(params)),
            Some(serde_json::json!("scalar")),
            "non-object extra_body must win outright over the injected thinking object"
        );
    }

    #[test]
    fn thinking_passthrough_forces_temperature_in_native_tool_builder() {
        // Anthropic rejects extended thinking combined with a modified
        // temperature: whenever the builder injects the thinking object, the
        // caller's temperature must be replaced with 1.0 (the native
        // provider's rule). Flag off and params-None keep the caller's
        // value, leaving those bodies byte-identical.
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };
        let messages = vec![ChatMessage::user("hello")];

        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();

        let req = p.build_native_tool_chat_request(
            &messages,
            None,
            "test-model",
            Some(0.75),
            false,
            false,
            Some(params),
        );
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value["temperature"],
            serde_json::json!(1.0),
            "injected thinking requires temperature 1.0; got: {value}"
        );
        assert_eq!(
            value["thinking"],
            serde_json::json!({"type": "enabled", "budget_tokens": 8_192}),
            "the forcing must ride on an actually injected thinking object"
        );

        let flag_off = make_model_provider("gateway", "http://localhost:8000/v1", None);
        let legacy = serde_json::to_value(flag_off.build_native_tool_chat_request(
            &messages,
            None,
            "test-model",
            Some(0.75),
            false,
            false,
            Some(params),
        ))
        .unwrap();
        assert_eq!(
            legacy["temperature"],
            serde_json::json!(0.75),
            "flag-off request must keep the caller's temperature"
        );
        assert!(
            legacy.get("thinking").is_none(),
            "flag-off request must inject nothing; got: {legacy}"
        );

        let no_params = serde_json::to_value(p.build_native_tool_chat_request(
            &messages,
            None,
            "test-model",
            Some(0.75),
            false,
            false,
            None,
        ))
        .unwrap();
        assert_eq!(
            no_params["temperature"],
            serde_json::json!(0.75),
            "params-None request under passthrough must keep the caller's temperature"
        );
    }

    #[test]
    fn thinking_passthrough_forces_temperature_in_raw_tool_builder() {
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };
        let messages = vec![ChatMessage::user("hello")];

        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();

        let req = p.build_raw_native_tool_chat_request(
            &messages,
            None,
            "test-model",
            Some(0.75),
            false,
            false,
            Some(params),
        );
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value["temperature"],
            serde_json::json!(1.0),
            "raw tool builder must force temperature 1.0 when thinking is injected"
        );
        assert!(value.get("thinking").is_some());

        let flag_off = make_model_provider("gateway", "http://localhost:8000/v1", None);
        let legacy = serde_json::to_value(flag_off.build_raw_native_tool_chat_request(
            &messages,
            None,
            "test-model",
            Some(0.75),
            false,
            false,
            Some(params),
        ))
        .unwrap();
        assert_eq!(
            legacy["temperature"],
            serde_json::json!(0.75),
            "flag-off raw tool request must keep the caller's temperature"
        );
        assert!(legacy.get("thinking").is_none());

        let no_params = serde_json::to_value(p.build_raw_native_tool_chat_request(
            &messages,
            None,
            "test-model",
            Some(0.75),
            false,
            false,
            None,
        ))
        .unwrap();
        assert_eq!(
            no_params["temperature"],
            serde_json::json!(0.75),
            "params-None raw tool request must keep the caller's temperature"
        );
    }

    #[test]
    fn thinking_passthrough_forces_temperature_for_adaptive_style() {
        // The forcing rule is style-independent, matching the native
        // provider: an adaptive-only gateway model with thinking params also
        // gets temperature 1.0.
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let messages = vec![ChatMessage::user("hello")];

        let req = p.build_native_tool_chat_request(
            &messages,
            None,
            "claude-group/claude-fable-5-1",
            Some(0.75),
            false,
            false,
            Some(params),
        );
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value["temperature"],
            serde_json::json!(1.0),
            "adaptive-style thinking must force temperature 1.0 too"
        );
        assert_eq!(
            value["thinking"],
            serde_json::json!({"type": "adaptive"}),
            "the adaptive shape must still be the injected object"
        );
    }

    #[test]
    fn thinking_passthrough_keeps_temperature_on_streaming_builder() {
        // Passthrough never attaches thinking to the streamed wire
        // (streaming_thinking_params): the temperature resolves from the
        // same params the builder actually injects, so the streamed body
        // behaves like thinking-off for the temperature as well: the
        // caller's value is kept, nothing is forced. Flag off is the same
        // byte-identical legacy body.
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };
        let messages = vec![ChatMessage::user("hello")];

        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();

        let streamed = serde_json::to_value(p.build_streaming_native_tool_request(
            "test-model",
            &messages,
            Some(vec![]),
            Some(0.75),
            true,
            false,
            false,
            Some(params),
        ))
        .unwrap();
        assert_eq!(
            streamed["temperature"],
            serde_json::json!(0.75),
            "streamed requests under passthrough carry no thinking object, so the caller's temperature is kept"
        );
        assert!(
            streamed.get("thinking").is_none(),
            "streamed request must inject nothing; got: {streamed}"
        );

        let flag_off = make_model_provider("gateway", "http://localhost:8000/v1", None);
        let legacy = serde_json::to_value(flag_off.build_streaming_native_tool_request(
            "test-model",
            &messages,
            Some(vec![]),
            Some(0.75),
            true,
            false,
            false,
            Some(params),
        ))
        .unwrap();
        assert_eq!(
            legacy["temperature"],
            serde_json::json!(0.75),
            "flag-off streamed request must keep the caller's temperature"
        );
        assert!(legacy.get("thinking").is_none());
    }

    #[tokio::test]
    async fn thinking_passthrough_forces_temperature_on_history_fallback() {
        // The prompt-guided fallback rebuilds the request through
        // chat_with_history_inner with the runtime's thinking params: the
        // rebuilt body must carry the same temperature forcing as the
        // primary path.
        use axum::Json;
        use axum::Router;
        use axum::routing::post;
        use std::sync::{Arc, Mutex};
        use tokio::net::TcpListener;

        let bodies = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let bodies_for_route = Arc::clone(&bodies);
        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(body): Json<serde_json::Value>| {
                let bodies = Arc::clone(&bodies_for_route);
                async move {
                    bodies.lock().unwrap().push(body);
                    Json(serde_json::json!({
                        "choices": [{"message": {"content": "ok"}}]
                    }))
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{addr}");
        let messages = vec![ChatMessage::user("hello")];
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };

        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url(&base_url)
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        p.chat_with_history_inner(&messages, "test-model", Some(0.75), Some(params))
            .await
            .unwrap();

        let flag_off = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url(&base_url)
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .build();
        flag_off
            .chat_with_history_inner(&messages, "test-model", Some(0.75), Some(params))
            .await
            .unwrap();

        p.chat_with_history_inner(&messages, "test-model", Some(0.75), None)
            .await
            .unwrap();

        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 3, "one captured body per request");
        assert_eq!(
            bodies[0]["temperature"],
            serde_json::json!(1.0),
            "fallback request with injected thinking must carry temperature 1.0"
        );
        assert!(bodies[0].get("thinking").is_some());
        assert_eq!(
            bodies[1]["temperature"],
            serde_json::json!(0.75),
            "flag-off fallback request must keep the caller's temperature"
        );
        assert!(bodies[1].get("thinking").is_none());
        assert_eq!(
            bodies[2]["temperature"],
            serde_json::json!(0.75),
            "params-None fallback request must keep the caller's temperature"
        );
        drop(bodies);
        server.abort();
    }

    #[test]
    fn thinking_passthrough_raises_max_tokens_in_native_tool_builder() {
        // Anthropic rejects a fixed-budget thinking request whose output
        // limit does not strictly exceed the budget: whenever the builder
        // injects the thinking object, the configured limit is raised to
        // budget_tokens + 1 (the native provider's rule). Flag off and
        // params-None keep the configured limit, leaving those bodies
        // byte-identical.
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };
        let messages = vec![ChatMessage::user("hello")];

        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .max_tokens(Some(4_096))
            .build();

        let req = p.build_native_tool_chat_request(
            &messages,
            None,
            "test-model",
            Some(0.75),
            false,
            false,
            Some(params),
        );
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value["max_tokens"],
            serde_json::json!(8_193),
            "injected thinking requires max_tokens above the budget; got: {value}"
        );
        assert_eq!(
            value["thinking"],
            serde_json::json!({"type": "enabled", "budget_tokens": 8_192}),
            "the raising must ride on an actually injected thinking object"
        );

        let flag_off = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .max_tokens(Some(4_096))
            .build();
        let legacy = serde_json::to_value(flag_off.build_native_tool_chat_request(
            &messages,
            None,
            "test-model",
            Some(0.75),
            false,
            false,
            Some(params),
        ))
        .unwrap();
        assert_eq!(
            legacy["max_tokens"],
            serde_json::json!(4_096),
            "flag-off request must keep the configured max_tokens"
        );
        assert!(
            legacy.get("thinking").is_none(),
            "flag-off request must inject nothing; got: {legacy}"
        );

        let no_params = serde_json::to_value(p.build_native_tool_chat_request(
            &messages,
            None,
            "test-model",
            Some(0.75),
            false,
            false,
            None,
        ))
        .unwrap();
        assert_eq!(
            no_params["max_tokens"],
            serde_json::json!(4_096),
            "params-None request under passthrough must keep the configured max_tokens"
        );
    }

    #[test]
    fn thinking_passthrough_raises_max_tokens_in_raw_tool_builder() {
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };
        let messages = vec![ChatMessage::user("hello")];

        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .max_tokens(Some(4_096))
            .build();

        let req = p.build_raw_native_tool_chat_request(
            &messages,
            None,
            "test-model",
            Some(0.75),
            false,
            false,
            Some(params),
        );
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value["max_tokens"],
            serde_json::json!(8_193),
            "raw tool builder must raise max_tokens when thinking is injected"
        );
        assert!(value.get("thinking").is_some());

        let flag_off = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .max_tokens(Some(4_096))
            .build();
        let legacy = serde_json::to_value(flag_off.build_raw_native_tool_chat_request(
            &messages,
            None,
            "test-model",
            Some(0.75),
            false,
            false,
            Some(params),
        ))
        .unwrap();
        assert_eq!(
            legacy["max_tokens"],
            serde_json::json!(4_096),
            "flag-off raw tool request must keep the configured max_tokens"
        );
        assert!(legacy.get("thinking").is_none());

        let no_params = serde_json::to_value(p.build_raw_native_tool_chat_request(
            &messages,
            None,
            "test-model",
            Some(0.75),
            false,
            false,
            None,
        ))
        .unwrap();
        assert_eq!(
            no_params["max_tokens"],
            serde_json::json!(4_096),
            "params-None raw tool request must keep the configured max_tokens"
        );
    }

    #[test]
    fn thinking_passthrough_keeps_max_tokens_when_configured_above_budget() {
        // The native rule raises only when needed: a configured limit
        // already above the budget is sent unchanged, and an unset limit
        // resolves to the minimum the budget requires.
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };
        let messages = vec![ChatMessage::user("hello")];

        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .max_tokens(Some(16_384))
            .build();
        let value = serde_json::to_value(p.build_native_tool_chat_request(
            &messages,
            None,
            "test-model",
            Some(0.75),
            false,
            false,
            Some(params),
        ))
        .unwrap();
        assert_eq!(
            value["max_tokens"],
            serde_json::json!(16_384),
            "configured limit above the budget must be kept"
        );
        assert!(value.get("thinking").is_some());

        let unset = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let value = serde_json::to_value(unset.build_native_tool_chat_request(
            &messages,
            None,
            "test-model",
            Some(0.75),
            false,
            false,
            Some(params),
        ))
        .unwrap();
        assert_eq!(
            value["max_tokens"],
            serde_json::json!(8_193),
            "unset limit must resolve to the budget minimum"
        );
    }

    #[test]
    fn thinking_passthrough_keeps_max_tokens_for_adaptive_style() {
        // The raising rule is budget-only, matching the native provider:
        // adaptive-style thinking carries no budget, so the configured
        // limit is unconstrained.
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .max_tokens(Some(4_096))
            .build();
        let messages = vec![ChatMessage::user("hello")];

        let req = p.build_native_tool_chat_request(
            &messages,
            None,
            "claude-group/claude-fable-5-1",
            Some(0.75),
            false,
            false,
            Some(params),
        );
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value["max_tokens"],
            serde_json::json!(4_096),
            "adaptive-style thinking must keep the configured max_tokens"
        );
        assert_eq!(
            value["thinking"],
            serde_json::json!({"type": "adaptive"}),
            "the adaptive shape must still be the injected object"
        );
    }

    #[test]
    fn thinking_passthrough_keeps_max_tokens_on_streaming_builder() {
        // Passthrough never attaches thinking to the streamed wire
        // (streaming_thinking_params): the limit resolves from the same
        // params the builder actually injects, so the streamed body
        // behaves like thinking-off for the limit as well: the configured
        // value is kept, nothing is raised. Flag off is the same
        // byte-identical legacy body.
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };
        let messages = vec![ChatMessage::user("hello")];

        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .max_tokens(Some(4_096))
            .build();

        let streamed = serde_json::to_value(p.build_streaming_native_tool_request(
            "test-model",
            &messages,
            Some(vec![]),
            Some(0.75),
            true,
            false,
            false,
            Some(params),
        ))
        .unwrap();
        assert_eq!(
            streamed["max_tokens"],
            serde_json::json!(4_096),
            "streamed requests under passthrough carry no thinking object, so the configured max_tokens is kept"
        );
        assert!(
            streamed.get("thinking").is_none(),
            "streamed request must inject nothing; got: {streamed}"
        );

        let flag_off = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("http://localhost:8000/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .max_tokens(Some(4_096))
            .build();
        let legacy = serde_json::to_value(flag_off.build_streaming_native_tool_request(
            "test-model",
            &messages,
            Some(vec![]),
            Some(0.75),
            true,
            false,
            false,
            Some(params),
        ))
        .unwrap();
        assert_eq!(
            legacy["max_tokens"],
            serde_json::json!(4_096),
            "flag-off streamed request must keep the configured max_tokens"
        );
        assert!(legacy.get("thinking").is_none());
    }

    #[tokio::test]
    async fn thinking_passthrough_raises_max_tokens_on_history_fallback() {
        // The prompt-guided fallback rebuilds the request through
        // chat_with_history_inner with the runtime's thinking params: the
        // rebuilt body must carry the same budget-safe limit as the
        // primary path.
        use axum::Json;
        use axum::Router;
        use axum::routing::post;
        use std::sync::{Arc, Mutex};
        use tokio::net::TcpListener;

        let bodies = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let bodies_for_route = Arc::clone(&bodies);
        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(body): Json<serde_json::Value>| {
                let bodies = Arc::clone(&bodies_for_route);
                async move {
                    bodies.lock().unwrap().push(body);
                    Json(serde_json::json!({
                        "choices": [{"message": {"content": "ok"}}]
                    }))
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{addr}");
        let messages = vec![ChatMessage::user("hello")];
        let params = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };

        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url(&base_url)
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .max_tokens(Some(4_096))
            .build();
        p.chat_with_history_inner(&messages, "test-model", Some(0.75), Some(params))
            .await
            .unwrap();

        let flag_off = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url(&base_url)
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .max_tokens(Some(4_096))
            .build();
        flag_off
            .chat_with_history_inner(&messages, "test-model", Some(0.75), Some(params))
            .await
            .unwrap();

        p.chat_with_history_inner(&messages, "test-model", Some(0.75), None)
            .await
            .unwrap();

        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 3, "one captured body per request");
        assert_eq!(
            bodies[0]["max_tokens"],
            serde_json::json!(8_193),
            "fallback request with injected thinking must carry a budget-safe limit"
        );
        assert!(bodies[0].get("thinking").is_some());
        assert_eq!(
            bodies[1]["max_tokens"],
            serde_json::json!(4_096),
            "flag-off fallback request must keep the configured max_tokens"
        );
        assert!(bodies[1].get("thinking").is_none());
        assert_eq!(
            bodies[2]["max_tokens"],
            serde_json::json!(4_096),
            "params-None fallback request must keep the configured max_tokens"
        );
        drop(bodies);
        server.abort();
    }

    #[test]
    fn build_native_tool_chat_request_omits_tool_choice_when_no_tools() {
        let p = make_model_provider("vllm", "http://localhost:8000/v1", None);
        let messages = vec![ChatMessage::user("hello")];

        // Assert on the structured value rather than substring-matching the
        // serialized string: a JSON-shape or escaping change could otherwise
        // flip these assertions silently. Inspect the `tool_choice` key
        // directly.

        // None tools → no tool_choice key.
        let req = p.build_native_tool_chat_request(
            &messages,
            None,
            "test-model",
            None,
            false,
            false,
            None,
        );
        let value = serde_json::to_value(&req).unwrap();
        assert!(
            value.get("tool_choice").is_none(),
            "tool_choice must be omitted when tools is None; got: {value}"
        );

        // Empty tools vec → still no tool_choice key.
        let req_empty = p.build_native_tool_chat_request(
            &messages,
            Some(vec![]),
            "test-model",
            None,
            false,
            false,
            None,
        );
        let value_empty = serde_json::to_value(&req_empty).unwrap();
        assert!(
            value_empty.get("tool_choice").is_none(),
            "tool_choice must be omitted when tools is empty; got: {value_empty}"
        );
    }

    #[test]
    fn build_native_tool_chat_request_sets_tool_choice_when_tools_present() {
        let p = make_model_provider("vllm", "http://localhost:8000/v1", None);
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![NativeToolSpec {
            kind: "function".to_string(),
            extra: serde_json::Map::new(),
            function: NativeToolFunctionSpec {
                extra: serde_json::Map::new(),
                name: "get_weather".to_string(),
                description: String::new(),
                parameters: std::sync::Arc::new(serde_json::json!({})),
            },
        }];
        let req = p.build_native_tool_chat_request(
            &messages,
            Some(tools),
            "test-model",
            None,
            false,
            false,
            None,
        );
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value.get("tool_choice").and_then(serde_json::Value::as_str),
            Some("auto"),
            "tool_choice must be 'auto' when tools are present; got: {value}"
        );
    }

    // A compatible endpoint may support tools and reasoning together. Preserve
    // the operator-selected effort on the first request; the bounded HTTP
    // fallback handles only endpoints that explicitly reject the combination.
    #[test]
    fn build_native_tool_chat_request_keeps_reasoning_effort_when_tools_present() {
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .reasoning_effort(Some("high".to_string()))
            .build();
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![NativeToolSpec {
            kind: "function".to_string(),
            extra: serde_json::Map::new(),
            function: NativeToolFunctionSpec {
                extra: serde_json::Map::new(),
                name: "get_weather".to_string(),
                description: String::new(),
                parameters: std::sync::Arc::new(serde_json::json!({})),
            },
        }];

        let req = p.build_native_tool_chat_request(
            &messages,
            Some(tools),
            "gpt-5",
            None,
            false,
            false,
            None,
        );
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value
                .get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some("high"),
            "capable endpoints must receive reasoning_effort with tools on the first request; got: {value}"
        );
    }

    // Regression guard: the no-tools path must keep sending reasoning_effort
    // for models that qualify, so the tool-bearing fix above doesn't
    // regress the common case.
    #[test]
    fn build_native_tool_chat_request_keeps_reasoning_effort_when_no_tools() {
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .reasoning_effort(Some("high".to_string()))
            .build();
        let messages = vec![ChatMessage::user("hello")];

        let req =
            p.build_native_tool_chat_request(&messages, None, "gpt-5", None, false, false, None);
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value
                .get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some("high"),
            "reasoning_effort must be present when no tools are sent; got: {value}"
        );
    }

    // Opt-in reasoning-effort passthrough: the name filter is fail-closed
    // because some backends reject unknown request params, so only an
    // explicit per-provider flag forwards effort to non-OpenAI model names.
    #[test]
    fn reasoning_effort_passthrough_default_keeps_name_filter_for_glm_style_models() {
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .reasoning_effort(Some("high".to_string()))
            .build();
        let messages = vec![ChatMessage::user("hello")];

        let req = p.build_native_tool_chat_request(
            &messages,
            None,
            "fireworks-primary/glm-5p3",
            None,
            false,
            false,
        );
        let value = serde_json::to_value(&req).unwrap();
        assert!(
            value.get("reasoning_effort").is_none(),
            "flag unset must preserve the name filter: glm-style models never receive reasoning_effort; got: {value}"
        );
    }

    #[test]
    fn reasoning_effort_passthrough_forwards_effort_to_glm_style_models() {
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .reasoning_effort(Some("high".to_string()))
            .with_reasoning_effort_passthrough()
            .build();
        let messages = vec![ChatMessage::user("hello")];

        let req = p.build_native_tool_chat_request(
            &messages,
            None,
            "fireworks-primary/glm-5p3",
            None,
            false,
            false,
        );
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value
                .get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some("high"),
            "flag on must forward the configured effort to glm-style models; got: {value}"
        );

        // Models the filter already passes keep the same outcome with the
        // flag on: passthrough only widens coverage, it never narrows it.
        let req =
            p.build_native_tool_chat_request(&messages, None, "openai/o3-mini", None, false, false);
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value
                .get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some("high"),
            "o3-style models must keep receiving the effort with the flag on; got: {value}"
        );
    }

    #[test]
    fn reasoning_effort_passthrough_without_configured_effort_sends_none() {
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_reasoning_effort_passthrough()
            .build();
        let messages = vec![ChatMessage::user("hello")];

        let req = p.build_native_tool_chat_request(
            &messages,
            None,
            "fireworks-primary/glm-5p3",
            None,
            false,
            false,
        );
        let value = serde_json::to_value(&req).unwrap();
        assert!(
            value.get("reasoning_effort").is_none(),
            "passthrough without a configured effort must not invent one; got: {value}"
        );
    }

    /// Spawn a mock `/chat/completions` endpoint that rejects any request
    /// carrying tools unless `reasoning_effort` is explicitly `"none"`.
    ///
    /// This mirrors endpoints that refuse function tools combined with any
    /// effective reasoning effort: because an omitted `reasoning_effort`
    /// defaults to a non-`none` effort server-side, an absent field is
    /// rejected just like `"high"`. Returns the bound address, the recorded
    /// request bodies, and the server task handle.
    ///
    /// `stream` selects an SSE success body instead of a JSON one.
    fn spawn_reasoning_rejecting_endpoint(
        stream: bool,
    ) -> impl std::future::Future<
        Output = (
            std::net::SocketAddr,
            std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
            ::tokio::task::JoinHandle<()>,
        ),
    > {
        use axum::Json;
        use axum::Router;
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::routing::post;
        use std::sync::{Arc, Mutex};
        use tokio::net::TcpListener;

        async move {
            let bodies = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
            let bodies_for_route = Arc::clone(&bodies);
            let app = Router::new().route(
                "/chat/completions",
                post(move |Json(body): Json<serde_json::Value>| {
                    let bodies = Arc::clone(&bodies_for_route);
                    async move {
                        let rejects = body.get("tools").is_some()
                            && body
                                .get("reasoning_effort")
                                .and_then(serde_json::Value::as_str)
                                != Some("none");
                        bodies.lock().unwrap().push(body);
                        if rejects {
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(serde_json::json!({
                                    "error": {
                                        "message": "Function tools with reasoning effort are not supported",
                                        "param": "reasoning_effort"
                                    }
                                })),
                            )
                                .into_response();
                        }
                        if stream {
                            return (
                                StatusCode::OK,
                                [("content-type", "text/event-stream")],
                                concat!(
                                    "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
                                    "data: [DONE]\n\n"
                                ),
                            )
                                .into_response();
                        }
                        Json(serde_json::json!({
                            "choices": [{"message": {"content": "ok"}}]
                        }))
                        .into_response()
                    }
                }),
            );
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = ::zeroclaw_spawn::spawn!(async move {
                axum::serve(listener, app).await.unwrap();
            });
            (addr, bodies, server)
        }
    }

    #[tokio::test]
    async fn capable_endpoint_keeps_reasoning_effort_with_tools_without_retry() {
        use axum::Json;
        use axum::Router;
        use axum::routing::post;
        use std::sync::{Arc, Mutex};
        use tokio::net::TcpListener;

        let bodies = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let bodies_for_route = Arc::clone(&bodies);
        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(body): Json<serde_json::Value>| {
                let bodies = Arc::clone(&bodies_for_route);
                async move {
                    bodies.lock().unwrap().push(body);
                    Json(serde_json::json!({
                        "choices": [{"message": {"content": "ok"}}]
                    }))
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url(&format!("http://{addr}"))
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .reasoning_effort(Some("high".to_string()))
            .build();
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![zeroclaw_api::tool::ToolSpec::new(
            "get_weather",
            "Get weather",
            serde_json::json!({"type": "object", "properties": {}}),
        )];

        let response = provider
            .chat(
                crate::traits::ChatRequest {
                    messages: &messages,
                    tools: Some(&tools),
                    thinking: None,
                },
                "gpt-5",
                None,
            )
            .await
            .unwrap();
        assert_eq!(response.text.as_deref(), Some("ok"));

        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1, "capable endpoint must not be retried");
        assert_eq!(
            bodies[0]
                .get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some("high")
        );
        assert!(bodies[0].get("tools").is_some());
        server.abort();
    }

    /// Spawn a mock endpoint that rejects any request carrying `tools` with a
    /// schema-unsupported error and accepts the tool-less fallback. Returns
    /// the bound address, the recorded bodies, and the server handle.
    fn spawn_schema_rejecting_endpoint() -> impl std::future::Future<
        Output = (
            std::net::SocketAddr,
            std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
            ::tokio::task::JoinHandle<()>,
        ),
    > {
        use axum::Json;
        use axum::Router;
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::routing::post;
        use std::sync::{Arc, Mutex};
        use tokio::net::TcpListener;

        async move {
            let bodies = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
            let bodies_for_route = Arc::clone(&bodies);
            let app = Router::new().route(
                "/chat/completions",
                post(move |Json(body): Json<serde_json::Value>| {
                    let bodies = Arc::clone(&bodies_for_route);
                    async move {
                        let rejects = body.get("tools").is_some();
                        bodies.lock().unwrap().push(body);
                        if rejects {
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(serde_json::json!({
                                    "error": {
                                        "message": "unknown parameter: tools",
                                    }
                                })),
                            )
                                .into_response();
                        }
                        Json(serde_json::json!({
                            "choices": [{
                                "message": {
                                    "content": "ok",
                                    "thinking_blocks": [
                                        {
                                            "type": "thinking",
                                            "thinking": "guided fallback thought",
                                            "signature": "sig_fb"
                                        }
                                    ]
                                }
                            }]
                        }))
                        .into_response()
                    }
                }),
            );
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = ::zeroclaw_spawn::spawn!(async move {
                axum::serve(listener, app).await.expect("serve schema test");
            });
            (addr, bodies, server)
        }
    }

    /// Schema-rejecting endpoint whose tool-less fallback responses carry
    /// signed thinking blocks, so the first fallback turn produces captured
    /// reasoning for the second turn's history.
    fn spawn_schema_rejecting_endpoint_with_signed_fallback() -> impl std::future::Future<
        Output = (
            std::net::SocketAddr,
            std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
            ::tokio::task::JoinHandle<()>,
        ),
    > {
        use axum::Json;
        use axum::Router;
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::routing::post;
        use std::sync::{Arc, Mutex};
        use tokio::net::TcpListener;

        async move {
            let bodies = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
            let bodies_for_route = Arc::clone(&bodies);
            let app = Router::new().route(
                "/chat/completions",
                post(move |Json(body): Json<serde_json::Value>| {
                    let bodies = Arc::clone(&bodies_for_route);
                    async move {
                        let rejects = body.get("tools").is_some();
                        bodies.lock().unwrap().push(body);
                        if rejects {
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(serde_json::json!({
                                    "error": { "message": "unknown parameter: tools" }
                                })),
                            )
                                .into_response();
                        }
                        Json(serde_json::json!({
                            "choices": [{
                                "message": {
                                    "content": "ok",
                                    "thinking_blocks": [
                                        {"type": "thinking", "thinking": "visible", "signature": "sig1"},
                                        {"type": "thinking", "thinking": "", "signature": "sig-only"},
                                        {"type": "redacted_thinking", "data": "ErUBCkEIRAP...opaque"}
                                    ]
                                }
                            }]
                        }))
                        .into_response()
                    }
                }),
            );
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = ::zeroclaw_spawn::spawn!(async move {
                axum::serve(listener, app)
                    .await
                    .expect("serve signed fallback test");
            });
            (addr, bodies, server)
        }
    }

    /// Schema-rejecting endpoint whose tool-less fallback reply carries a
    /// native tool call beside visible content, signed thinking, and usage:
    /// the prompt-guided fallback retry is not limited to text-only replies.
    fn spawn_schema_rejecting_endpoint_with_tool_call_fallback() -> impl std::future::Future<
        Output = (
            std::net::SocketAddr,
            std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
            ::tokio::task::JoinHandle<()>,
        ),
    > {
        use axum::Json;
        use axum::Router;
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::routing::post;
        use std::sync::{Arc, Mutex};
        use tokio::net::TcpListener;

        async move {
            let bodies = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
            let bodies_for_route = Arc::clone(&bodies);
            let app = Router::new().route(
                "/chat/completions",
                post(move |Json(body): Json<serde_json::Value>| {
                    let bodies = Arc::clone(&bodies_for_route);
                    async move {
                        let rejects = body.get("tools").is_some();
                        bodies.lock().unwrap().push(body);
                        if rejects {
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(serde_json::json!({
                                    "error": { "message": "unknown parameter: tools" }
                                })),
                            )
                                .into_response();
                        }
                        Json(serde_json::json!({
                            "choices": [{
                                "message": {
                                    "content": "ok",
                                    "tool_calls": [{
                                        "id": "call_1",
                                        "type": "function",
                                        "function": {
                                            "name": "get_weather",
                                            "arguments": "{\"city\":\"SF\"}"
                                        }
                                    }],
                                    "thinking_blocks": [
                                        {"type": "thinking", "thinking": "fallback thought", "signature": "sig_norm"}
                                    ]
                                }
                            }],
                            "usage": {"prompt_tokens": 12, "completion_tokens": 34}
                        }))
                        .into_response()
                    }
                }),
            );
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = ::zeroclaw_spawn::spawn!(async move {
                axum::serve(listener, app)
                    .await
                    .expect("serve tool-call fallback test");
            });
            (addr, bodies, server)
        }
    }

    #[tokio::test]
    async fn second_schema_fallback_replays_signed_history() {
        let (addr, bodies, server) = spawn_schema_rejecting_endpoint_with_signed_fallback().await;

        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url(&format!("http://{addr}"))
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let tools = vec![zeroclaw_api::tool::ToolSpec::new(
            "get_weather",
            "Get weather",
            serde_json::json!({"type": "object", "properties": {}}),
        )];
        let thinking = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };

        // Turn 1: schema rejected, prompt-guided fallback succeeds and its
        // response carries signed thinking blocks, which capture converts
        // into replay lines.
        let turn_one = vec![ChatMessage::user("What is the weather in SF?")];
        let response = provider
            .chat(
                crate::traits::ChatRequest {
                    messages: &turn_one,
                    tools: Some(&tools),
                    thinking: Some(thinking),
                },
                "test-model",
                None,
            )
            .await
            .unwrap();
        let captured = response
            .reasoning_content
            .expect("fallback thinking captured");
        let captured_lines: Vec<&str> = captured.split('\n').collect();
        assert_eq!(captured_lines.len(), 3, "signed, signature-only, redacted");

        // Turn 2: history carries the runtime-persisted reasoning envelope;
        // the gateway rejects the schema again, so this request also goes
        // through the fallback history builder.
        let envelope = serde_json::json!({
            "content": "ok",
            "reasoning_content": captured,
        });
        let turn_two = vec![
            ChatMessage::user("What is the weather in SF?"),
            ChatMessage::assistant(envelope.to_string()),
            ChatMessage::user("And the forecast for tomorrow?"),
        ];
        provider
            .chat(
                crate::traits::ChatRequest {
                    messages: &turn_two,
                    tools: Some(&tools),
                    thinking: Some(thinking),
                },
                "test-model",
                None,
            )
            .await
            .unwrap();

        let bodies = bodies.lock().unwrap();
        // Each chat call records its rejected primary request plus its
        // fallback request: [primary-1, fallback-1, primary-2, fallback-2].
        assert_eq!(bodies.len(), 4, "two calls, primary plus fallback each");
        assert!(
            bodies[1]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|message| message.get("thinking_blocks").is_none()),
            "turn-1 history carries no reasoning yet"
        );

        let assistant = bodies[3]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "assistant")
            .expect("assistant turn in second request");
        assert_eq!(
            assistant["thinking_blocks"],
            serde_json::json!([
                {"type": "thinking", "thinking": "visible", "signature": "sig1"},
                {"type": "thinking", "thinking": "", "signature": "sig-only"},
                {"type": "redacted_thinking", "data": "ErUBCkEIRAP...opaque"}
            ]),
            "the second fallback request must replay the exact signed blocks"
        );
        assert_eq!(
            bodies[3]["thinking"],
            serde_json::json!({"type": "enabled", "budget_tokens": 8_192}),
            "fallback request keeps the thinking request object"
        );
        server.abort();
    }

    #[tokio::test]
    async fn fallback_history_replay_is_flag_gated() {
        let (addr, bodies, server) = spawn_schema_rejecting_endpoint_with_signed_fallback().await;

        // Same envelope history, flag OFF: the fallback wire must stay
        // byte-identical to the pre-passthrough behavior, so the assistant
        // message carries no thinking_blocks field at all.
        let provider = make_model_provider("test", &format!("http://{addr}"), None);
        let tools = vec![zeroclaw_api::tool::ToolSpec::new(
            "get_weather",
            "Get weather",
            serde_json::json!({"type": "object", "properties": {}}),
        )];
        let messages = vec![
            ChatMessage::user("What is the weather in SF?"),
            ChatMessage::assistant(
                serde_json::json!({
                    "content": "ok",
                    "reasoning_content": "{\"thinking\":\"visible\",\"signature\":\"sig1\"}"
                })
                .to_string(),
            ),
            ChatMessage::user("And the forecast?"),
        ];

        provider
            .chat(
                crate::traits::ChatRequest {
                    messages: &messages,
                    tools: Some(&tools),
                    thinking: None,
                },
                "test-model",
                None,
            )
            .await
            .unwrap();

        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2, "primary plus fallback");
        let assistant = bodies[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "assistant")
            .expect("assistant turn");
        assert!(
            assistant.get("thinking_blocks").is_none(),
            "flag-off fallback wire must not grow a thinking_blocks field"
        );
        server.abort();
    }

    #[tokio::test]
    async fn schema_fallback_rethreads_thinking_params() {
        let (addr, bodies, server) = spawn_schema_rejecting_endpoint().await;

        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url(&format!("http://{addr}"))
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![zeroclaw_api::tool::ToolSpec::new(
            "get_weather",
            "Get weather",
            serde_json::json!({"type": "object", "properties": {}}),
        )];
        let thinking = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };

        let response = provider
            .chat(
                crate::traits::ChatRequest {
                    messages: &messages,
                    tools: Some(&tools),
                    thinking: Some(thinking),
                },
                "test-model",
                None,
            )
            .await
            .unwrap();
        assert_eq!(response.text.as_deref(), Some("ok"));
        let captured: serde_json::Value = serde_json::from_str(
            response
                .reasoning_content
                .as_deref()
                .expect("fallback response must capture the returned signed thinking"),
        )
        .unwrap();
        assert_eq!(
            captured,
            serde_json::json!({
                "thinking": "guided fallback thought",
                "signature": "sig_fb"
            }),
            "fallback response must capture the returned signed thinking, not drop it"
        );

        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2, "schema fallback must be a single retry");
        assert_eq!(
            bodies[0]["thinking"],
            serde_json::json!({"type": "enabled", "budget_tokens": 8_192}),
            "primary request carries the injected thinking object"
        );
        assert!(bodies[0].get("tools").is_some());
        assert!(
            bodies[1].get("tools").is_none(),
            "fallback rebuild drops the tools parameter"
        );
        assert_eq!(
            bodies[1]["thinking"],
            serde_json::json!({"type": "enabled", "budget_tokens": 8_192}),
            "fallback rebuild must rethread the same thinking injection"
        );
        drop(bodies);
        server.abort();

        // Two-turn regression: the captured fallback thinking must replay on
        // the next tool-loop iteration. Simulate turn 2 by appending the
        // assistant turn (as the runtime persists it) and converting.
        let assistant_content = serde_json::json!({
            "content": "ok",
            "reasoning_content": response.reasoning_content,
            "tool_calls": [
                {
                    "id": "call_1",
                    "name": "get_weather",
                    "arguments": "{\"city\": \"SF\"}",
                }
            ],
        });
        let history = vec![
            ChatMessage::user("hello"),
            ChatMessage::assistant(assistant_content.to_string()),
            ChatMessage::tool(r#"{"tool_call_id": "call_1", "content": "72F"}"#.to_string()),
        ];
        let native = provider.convert_messages_for_native(&history, false);
        assert_eq!(
            native[1].thinking_blocks,
            Some(vec![serde_json::json!({
                "type": "thinking",
                "thinking": "guided fallback thought",
                "signature": "sig_fb"
            })]),
            "turn-2 outbound must replay the exact signed blocks captured on the fallback turn"
        );
    }

    #[tokio::test]
    async fn schema_fallback_returns_normalized_text_beside_native_tool_calls() {
        let (addr, bodies, server) =
            spawn_schema_rejecting_endpoint_with_tool_call_fallback().await;

        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url(&format!("http://{addr}"))
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let tools = vec![zeroclaw_api::tool::ToolSpec::new(
            "get_weather",
            "Get weather",
            serde_json::json!({"type": "object", "properties": {}}),
        )];
        let thinking = zeroclaw_api::model_provider::NativeThinkingParams {
            budget_tokens: 8_192,
            display: None,
        };
        let messages = vec![ChatMessage::user("What is the weather in SF?")];

        let response = provider
            .chat(
                crate::traits::ChatRequest {
                    messages: &messages,
                    tools: Some(&tools),
                    thinking: Some(thinking),
                },
                "test-model",
                None,
            )
            .await
            .unwrap();

        assert_eq!(
            response.text.as_deref(),
            Some("ok"),
            "fallback text must stay the normalized content, not the legacy JSON projection"
        );
        assert!(
            !response
                .text
                .as_deref()
                .unwrap_or_default()
                .contains("tool_calls"),
            "visible narration must not carry the serialized message JSON"
        );
        assert_eq!(
            response.tool_calls.len(),
            1,
            "the native call must surface structurally"
        );
        assert_eq!(response.tool_calls[0].name, "get_weather");
        assert_eq!(response.tool_calls[0].arguments, "{\"city\":\"SF\"}");
        let captured: serde_json::Value = serde_json::from_str(
            response
                .reasoning_content
                .as_deref()
                .expect("fallback response must capture the returned signed thinking"),
        )
        .unwrap();
        assert_eq!(
            captured,
            serde_json::json!({
                "thinking": "fallback thought",
                "signature": "sig_norm"
            }),
            "fallback response must capture the returned signed thinking"
        );
        let usage = response.usage.expect("fallback response surfaces usage");
        assert_eq!(usage.input_tokens, Some(12));
        assert_eq!(usage.output_tokens, Some(34));

        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2, "schema fallback must be a single retry");
        drop(bodies);
        server.abort();
    }

    #[tokio::test]
    async fn chat_with_history_keeps_legacy_json_text_for_tool_call_responses() {
        let reply = serde_json::json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_legacy",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\":\"SF\"}"
                        }
                    }]
                }
            }]
        });
        let (provider, _captured, server) = mock_non_streaming_response(reply.clone()).await;
        let messages = vec![ChatMessage::user("What is the weather in SF?")];

        let text = provider
            .chat_with_history(&messages, "test-model", None)
            .await
            .expect("chat history should succeed");

        let parsed: serde_json::Value = serde_json::from_str(&text)
            .expect("tool-call replies keep the serialized-message JSON text contract");
        assert_eq!(
            parsed["tool_calls"][0]["function"]["name"],
            serde_json::json!("get_weather"),
            "legacy text names the called function"
        );
        let message: ResponseMessage =
            serde_json::from_value(reply["choices"][0]["message"].clone())
                .expect("reply message parses as the gateway message shape");
        assert_eq!(
            text,
            serde_json::to_string(&message).expect("gateway message serializes"),
            "legacy text is exactly the serialized gateway message"
        );

        server.abort();
    }

    #[tokio::test]
    async fn rejecting_endpoint_retries_once_with_reasoning_disabled() {
        let (addr, bodies, server) = spawn_reasoning_rejecting_endpoint(false).await;

        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url(&format!("http://{addr}"))
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .reasoning_effort(Some("high".to_string()))
            .extra_body(serde_json::json!({"reasoning_effort": "xhigh"}))
            .build();
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![zeroclaw_api::tool::ToolSpec::new(
            "get_weather",
            "Get weather",
            serde_json::json!({"type": "object", "properties": {}}),
        )];

        let response = provider
            .chat(
                crate::traits::ChatRequest {
                    messages: &messages,
                    tools: Some(&tools),
                    thinking: None,
                },
                "gpt-5",
                None,
            )
            .await
            .unwrap();
        assert_eq!(response.text.as_deref(), Some("ok"));

        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2, "fallback must be bounded to one retry");
        assert_eq!(
            bodies[0]
                .get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some("xhigh"),
            "provider extra body must be reflected in the canonical wire payload"
        );
        assert_eq!(
            bodies[1]
                .get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some("none"),
            "retry must explicitly disable reasoning"
        );
        assert!(bodies.iter().all(|body| body.get("tools").is_some()));
        server.abort();
    }

    #[tokio::test]
    async fn streaming_rejection_retries_once_with_reasoning_disabled() {
        use futures_util::StreamExt as _;

        let (addr, bodies, server) = spawn_reasoning_rejecting_endpoint(true).await;

        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url(&format!("http://{addr}"))
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .reasoning_effort(Some("high".to_string()))
            .build();
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![zeroclaw_api::tool::ToolSpec::new(
            "get_weather",
            "Get weather",
            serde_json::json!({"type": "object", "properties": {}}),
        )];

        let events = provider
            .stream_chat(
                crate::traits::ChatRequest {
                    messages: &messages,
                    tools: Some(&tools),
                    thinking: None,
                },
                "gpt-5",
                None,
                StreamOptions {
                    enabled: true,
                    count_tokens: false,
                },
            )
            .collect::<Vec<_>>()
            .await;
        assert!(events.iter().all(Result::is_ok));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                Ok(StreamEvent::TextDelta(StreamChunk { delta, .. })) if delta == "ok"
            )
        }));

        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2, "stream fallback must retry exactly once");
        assert_eq!(
            bodies[0]
                .get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some("high")
        );
        assert_eq!(
            bodies[1]
                .get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some("none")
        );
        assert!(bodies.iter().all(|body| body.get("tools").is_some()));
        server.abort();
    }

    #[tokio::test]
    async fn chat_with_tools_retries_once_with_reasoning_disabled() {
        let (addr, bodies, server) = spawn_reasoning_rejecting_endpoint(false).await;

        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url(&format!("http://{addr}"))
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .reasoning_effort(Some("high".to_string()))
            .build();
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get weather",
                "parameters": {"type": "object", "properties": {}}
            }
        })];

        let response = provider
            .chat_with_tools(&messages, &tools, "gpt-5", None)
            .await
            .unwrap();
        assert_eq!(response.text.as_deref(), Some("ok"));

        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2, "fallback must be bounded to one retry");
        assert_eq!(
            bodies[0]
                .get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some("high")
        );
        assert_eq!(
            bodies[1]
                .get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some("none")
        );
        assert!(bodies.iter().all(|body| body.get("tools").is_some()));
        server.abort();
    }

    #[tokio::test]
    async fn compatible_retries_once_inserting_reasoning_effort_none_when_unset() {
        // Regression: a model that receives no reasoning_effort at all (the
        // common case, since reasoning_effort_for_model returns None outside
        // GPT-5/o-series/codex) must still recover. An omitted field defaults
        // to a non-"none" effort server-side, so the retry has to *insert*
        // the explicit "none" rather than only overwrite an existing value.
        let (addr, bodies, server) = spawn_reasoning_rejecting_endpoint(false).await;

        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url(&format!("http://{addr}"))
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .reasoning_effort(None)
            .build();
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![zeroclaw_api::tool::ToolSpec::new(
            "get_weather",
            "Get weather",
            serde_json::json!({"type": "object", "properties": {}}),
        )];

        let response = provider
            .chat(
                crate::traits::ChatRequest {
                    messages: &messages,
                    tools: Some(&tools),
                    thinking: None,
                },
                "gpt-5",
                None,
            )
            .await
            .unwrap();
        assert_eq!(response.text.as_deref(), Some("ok"));

        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2, "fallback must be bounded to one retry");
        assert!(
            bodies[0].get("reasoning_effort").is_none(),
            "first request must omit reasoning_effort when none is configured; got: {}",
            bodies[0]
        );
        assert_eq!(
            bodies[1]
                .get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some("none"),
            "retry must insert an explicit reasoning_effort of none"
        );
        assert!(bodies.iter().all(|body| body.get("tools").is_some()));
        server.abort();
    }

    #[tokio::test]
    async fn compatible_streaming_retries_once_inserting_reasoning_effort_none_when_unset() {
        use futures_util::StreamExt as _;

        let (addr, bodies, server) = spawn_reasoning_rejecting_endpoint(true).await;

        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url(&format!("http://{addr}"))
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .reasoning_effort(None)
            .build();
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![zeroclaw_api::tool::ToolSpec::new(
            "get_weather",
            "Get weather",
            serde_json::json!({"type": "object", "properties": {}}),
        )];

        let events = provider
            .stream_chat(
                crate::traits::ChatRequest {
                    messages: &messages,
                    tools: Some(&tools),
                    thinking: None,
                },
                "gpt-5",
                None,
                StreamOptions {
                    enabled: true,
                    count_tokens: false,
                },
            )
            .collect::<Vec<_>>()
            .await;
        assert!(events.iter().all(Result::is_ok));

        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2, "stream fallback must retry exactly once");
        assert!(bodies[0].get("reasoning_effort").is_none());
        assert_eq!(
            bodies[1]
                .get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some("none")
        );
        server.abort();
    }

    #[tokio::test]
    async fn chat_with_tools_retries_once_inserting_reasoning_effort_none_when_unset() {
        let (addr, bodies, server) = spawn_reasoning_rejecting_endpoint(false).await;

        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url(&format!("http://{addr}"))
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .reasoning_effort(None)
            .build();
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get weather",
                "parameters": {"type": "object", "properties": {}}
            }
        })];

        let response = provider
            .chat_with_tools(&messages, &tools, "gpt-5", None)
            .await
            .unwrap();
        assert_eq!(response.text.as_deref(), Some("ok"));

        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2, "fallback must be bounded to one retry");
        assert!(bodies[0].get("reasoning_effort").is_none());
        assert_eq!(
            bodies[1]
                .get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some("none")
        );
        server.abort();
    }

    #[tokio::test]
    async fn bad_reasoning_value_naming_a_tool_model_does_not_retry() {
        // Interaction of both fixes: the endpoint reports a plain bad-value
        // error whose text happens to name a model containing "tool". The
        // classifier must not treat that as a tools conflict, so the
        // configured effort is preserved and the error propagates unchanged
        // after exactly one request.
        use axum::Json;
        use axum::Router;
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::routing::post;
        use std::sync::{Arc, Mutex};
        use tokio::net::TcpListener;

        let bodies = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let bodies_for_route = Arc::clone(&bodies);
        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(body): Json<serde_json::Value>| {
                let bodies = Arc::clone(&bodies_for_route);
                async move {
                    bodies.lock().unwrap().push(body);
                    (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({
                            "error": {
                                "message": "reasoning_effort value 'high' is unsupported for tool-model",
                                "param": "reasoning_effort"
                            }
                        })),
                    )
                        .into_response()
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url(&format!("http://{addr}"))
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .reasoning_effort(Some("high".to_string()))
            .build();
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![zeroclaw_api::tool::ToolSpec::new(
            "get_weather",
            "Get weather",
            serde_json::json!({"type": "object", "properties": {}}),
        )];

        let result = provider
            .chat(
                crate::traits::ChatRequest {
                    messages: &messages,
                    tools: Some(&tools),
                    thinking: None,
                },
                "gpt-5",
                None,
            )
            .await;
        assert!(result.is_err(), "bad-value errors must propagate unchanged");

        let bodies = bodies.lock().unwrap();
        assert_eq!(
            bodies.len(),
            1,
            "a bad-value error must not trigger the tools/reasoning fallback"
        );
        assert_eq!(
            bodies[0]
                .get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some("high"),
            "the operator-configured effort must not be downgraded"
        );
        server.abort();
    }

    #[test]
    fn ensure_reasoning_effort_none_normalizes_non_canonical_spellings() {
        // A case-sensitive endpoint can reject "NONE" supplied through
        // provider_extra, so a non-canonical spelling must be rewritten to the
        // exact lowercase value rather than counted as already repaired.
        for spelling in ["NONE", "None", "nOnE"] {
            let mut payload = serde_json::json!({ "reasoning_effort": spelling });
            assert!(
                ensure_reasoning_effort_none(&mut payload),
                "{spelling} must be normalized to the canonical value"
            );
            assert_eq!(
                payload.get("reasoning_effort"),
                Some(&serde_json::Value::String("none".to_string()))
            );
            // The rewritten payload is the fixed point, so the retry stays
            // bounded to a single additional request.
            assert!(!ensure_reasoning_effort_none(&mut payload));
        }

        let mut canonical = serde_json::json!({ "reasoning_effort": "none" });
        assert!(
            !ensure_reasoning_effort_none(&mut canonical),
            "exact lowercase none is the fixed point"
        );
    }

    #[tokio::test]
    async fn compatible_retry_normalizes_uppercase_reasoning_effort_on_the_wire() {
        // Wire-payload regression: an operator-supplied "NONE" reaches a
        // case-sensitive endpoint, is rejected, and the retry must carry the
        // canonical lowercase spelling.
        let (addr, bodies, server) = spawn_reasoning_rejecting_endpoint(false).await;

        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url(&format!("http://{addr}"))
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .reasoning_effort(Some("NONE".to_string()))
            .build();
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![zeroclaw_api::tool::ToolSpec::new(
            "get_weather",
            "Get weather",
            serde_json::json!({"type": "object", "properties": {}}),
        )];

        let response = provider
            .chat(
                crate::traits::ChatRequest {
                    messages: &messages,
                    tools: Some(&tools),
                    thinking: None,
                },
                "gpt-5",
                None,
            )
            .await
            .unwrap();
        assert_eq!(response.text.as_deref(), Some("ok"));

        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2, "fallback must be bounded to one retry");
        assert_eq!(
            bodies[0]
                .get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some("NONE"),
            "the first request preserves the operator-supplied spelling"
        );
        assert_eq!(
            bodies[1]
                .get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some("none"),
            "the retry must normalize to the canonical lowercase none"
        );
        assert!(bodies.iter().all(|body| body.get("tools").is_some()));
        server.abort();
    }

    #[test]
    fn strips_trailing_slash() {
        let p = make_model_provider("test", "https://example.com/", None);
        assert_eq!(p.base_url, "https://example.com");
    }

    #[test]
    fn with_tls_ca_cert_path_missing_file_leaves_pem_none() {
        // Regression: a non-existent cert path must not panic or propagate an
        // error — the provider falls back to system roots and logs a warning.
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .tls_ca_cert_path("/nonexistent/path/to/ca.pem")
            .build();
        assert!(
            p.tls_ca_cert_pem.is_none(),
            "missing cert file must leave tls_ca_cert_pem as None (fall back to system roots)"
        );
    }

    #[test]
    fn with_tls_ca_cert_path_invalid_pem_stores_bytes_and_http_client_still_builds() {
        // The path-read step stores raw bytes; PEM parsing happens in http_client().
        // Writing invalid PEM to a temp file: read succeeds (bytes stored), then
        // http_client() logs a WARN and falls back to system roots — no panic, no error.
        let path = format!("/tmp/zeroclaw-test-invalid-pem-{}.pem", std::process::id());
        std::fs::write(&path, b"not-a-valid-pem").unwrap();
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .tls_ca_cert_path(&path)
            .build();
        std::fs::remove_file(&path).ok();
        assert!(
            p.tls_ca_cert_pem.is_some(),
            "readable file (even with bad PEM) must populate tls_ca_cert_pem bytes"
        );
        // http_client() must build cleanly even when PEM parse fails internally.
        // The method returns Client directly (panics on builder error), so if we
        // reach here without panic the fallback-to-system-roots path is working.
        let _client = p.http_client();
    }

    #[test]
    fn with_tls_ca_cert_path_invalid_pem_streaming_http_client_still_builds() {
        // Streaming requests use a separate client builder, so the TLS override
        // must degrade the same way there: warn, use system roots, and keep going.
        let path = format!(
            "/tmp/zeroclaw-test-invalid-pem-streaming-{}.pem",
            std::process::id()
        );
        std::fs::write(&path, b"not-a-valid-pem").unwrap();
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .tls_ca_cert_path(&path)
            .build();
        std::fs::remove_file(&path).ok();
        assert!(
            p.tls_ca_cert_pem.is_some(),
            "readable file (even with bad PEM) must populate tls_ca_cert_pem bytes"
        );
        let _client = p.streaming_http_client();
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

    fn sse_response(body: &'static str) -> reqwest::Response {
        reqwest::Response::from(
            axum::http::Response::builder()
                .status(reqwest::StatusCode::OK)
                .body(reqwest::Body::from(body))
                .expect("test response should build"),
        )
    }

    async fn collect_stream_events(body: &'static str) -> Vec<StreamResult<StreamEvent>> {
        let mut stream = sse_bytes_to_events(sse_response(body), false);
        let mut events = Vec::new();
        while let Ok(Some(ev)) =
            tokio::time::timeout(std::time::Duration::from_secs(2), stream.next()).await
        {
            events.push(ev);
        }
        events
    }

    async fn open_sse_response(
        body: &'static str,
    ) -> (reqwest::Response, tokio::task::JoinHandle<()>) {
        use axum::{Router, response::IntoResponse, routing::get};

        let app = Router::new().route(
            "/stream",
            get(move || async move {
                let first = futures_util::stream::once(async move {
                    Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(
                        body.as_bytes(),
                    ))
                });
                let open = futures_util::stream::pending::<
                    Result<axum::body::Bytes, std::convert::Infallible>,
                >();
                axum::body::Body::from_stream(first.chain(open)).into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind SSE test server");
        let addr = listener.local_addr().expect("SSE test server address");
        let server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.expect("serve SSE test");
        });
        let response = reqwest::Client::new()
            .get(format!("http://{addr}/stream"))
            .send()
            .await
            .expect("request SSE test stream");
        (response, server)
    }

    #[tokio::test]
    async fn done_sentinel_finishes_chunk_stream_without_eof() {
        let (response, server) = open_sse_response(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n",
        )
        .await;
        let mut stream = sse_bytes_to_chunks(
            response,
            false,
            crate::StreamIdleBound::Fixed(crate::STREAM_IDLE_TIMEOUT),
        );

        let first = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .expect("text delta must arrive before the connection closes")
            .expect("chunk stream must yield text")
            .expect("text chunk must be valid");
        assert_eq!(first.delta, "hi");
        let final_chunk = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .expect("[DONE] must finish the stream without EOF")
            .expect("chunk stream must yield Final")
            .expect("Final chunk must be valid");
        assert!(final_chunk.is_final);
        server.abort();
    }

    #[tokio::test]
    async fn done_sentinel_finishes_event_stream_without_eof() {
        let (response, server) = open_sse_response(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n",
        )
        .await;
        let mut stream = sse_bytes_to_events(response, false);
        let mut saw_final = false;

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while let Some(event) = stream.next().await {
                if matches!(event, Ok(StreamEvent::Final)) {
                    saw_final = true;
                    break;
                }
            }
        })
        .await
        .expect("[DONE] must finish the event stream without EOF");

        server.abort();
        assert!(saw_final, "terminal sentinel must emit Final");
    }

    #[tokio::test]
    async fn eof_after_done_sentinel_emits_final() {
        let events = collect_stream_events(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n",
        )
        .await;
        assert!(
            matches!(events.last(), Some(Ok(StreamEvent::Final))),
            "got: {events:?}"
        );
    }

    #[tokio::test]
    async fn eof_after_finish_reason_without_done_emits_final() {
        let events = collect_stream_events(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n",
        )
        .await;
        assert!(
            matches!(events.last(), Some(Ok(StreamEvent::Final))),
            "got: {events:?}"
        );
    }

    #[tokio::test]
    async fn eof_before_completion_signal_surfaces_error_not_final() {
        let events =
            collect_stream_events("data: {\"choices\":[{\"delta\":{\"content\":\"par\"}}]}\n\n")
                .await;
        assert!(
            !events.iter().any(|e| matches!(e, Ok(StreamEvent::Final))),
            "truncated stream must not emit Final, got: {events:?}"
        );
        assert!(
            matches!(
                events.last(),
                Some(Err(StreamError::Http(msg))) if msg.contains("truncated")
            ),
            "expected truncation error, got: {events:?}"
        );
    }

    #[test]
    fn native_chat_request_with_tools_includes_stream_options() {
        // Regression: tool-enabled streaming requests must opt the response
        // into a final `usage` SSE event, otherwise OpenAI-compatible providers
        // never report token counts on the `/ws/chat` path (the gateway's
        // primary path uses native tools). See Audacity88'sreview.
        let req: NativeChatRequest = NativeChatRequest {
            model: "gpt-4o".to_string(),
            messages: vec![NativeMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("hello".to_string())),
                tool_call_id: None,
                tool_calls: None,
                reasoning_content: None,
                reasoning: None,
                thinking_blocks: None,
                name: None,
            }],
            temperature: Some(0.7),
            stream: Some(true),
            stream_options: Some(StreamOptionsBody {
                include_usage: true,
            }),
            reasoning_effort: None,
            tool_stream: None,
            tools: Some(vec![NativeToolSpec {
                kind: "function".to_string(),
                extra: serde_json::Map::new(),
                function: NativeToolFunctionSpec {
                    extra: serde_json::Map::new(),
                    name: "echo".to_string(),
                    description: String::new(),
                    parameters: std::sync::Arc::new(serde_json::json!({})),
                },
            }]),
            tool_choice: Some("auto".to_string()),
            max_tokens: None,
            extra_body: None,
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
        let req: NativeChatRequest = NativeChatRequest {
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
            extra_body: None,
        };
        let value: serde_json::Value = serde_json::to_value(&req).unwrap();
        assert!(
            value.get("stream_options").is_none(),
            "non-streaming NativeChatRequest must not emit a stream_options key"
        );
    }

    #[test]
    fn extra_body_flattens_into_request_top_level() {
        let req: NativeChatRequest = NativeChatRequest {
            model: "qwen".to_string(),
            messages: vec![],
            temperature: None,
            stream: None,
            stream_options: None,
            reasoning_effort: None,
            tool_stream: None,
            tools: None,
            tool_choice: None,
            max_tokens: None,
            extra_body: Some(serde_json::json!({"thinking": "off"})),
        };
        let value: serde_json::Value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value.get("thinking").and_then(serde_json::Value::as_str),
            Some("off"),
            "extra_body fields must serialize at the top level, not nested"
        );
        assert!(
            value.get("extra_body").is_none(),
            "extra_body key itself must not appear in serialized JSON"
        );
    }

    #[test]
    fn api_chat_request_flattens_extra_body_into_top_level() {
        // Regression: the no-tools request struct (`chat_with_system`,
        // `chat_with_history`, no-tools streaming) must also carry the
        // config-driven `extra_body`, not just the native-tools path.
        let req = ApiChatRequest {
            model: "qwen".to_string(),
            messages: vec![],
            temperature: None,
            stream: None,
            stream_options: None,
            reasoning_effort: None,
            tool_stream: None,
            tools: None,
            tool_choice: None,
            max_tokens: None,
            extra_body: Some(serde_json::json!({
                "top_p": 0.95,
                "chat_template_kwargs": {"thinking": true, "reasoning_effort": "max"},
            })),
        };
        let value: serde_json::Value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value.get("top_p").and_then(serde_json::Value::as_f64),
            Some(0.95),
            "provider_extra keys must serialize at the top level of a no-tools request"
        );
        assert_eq!(
            value.pointer("/chat_template_kwargs/reasoning_effort"),
            Some(&serde_json::json!("max")),
            "chat_template_kwargs must be nested under its own top-level key in a no-tools request"
        );
        assert!(
            value.get("extra_body").is_none(),
            "extra_body key itself must not appear in serialized JSON"
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
                    thinking_blocks: None,
                },
                Message {
                    role: "user".to_string(),
                    content: MessageContent::Text("hello".to_string()),
                    thinking_blocks: None,
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
            extra_body: None,
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

    #[tokio::test]
    async fn ordinary_non_streaming_path_uses_shared_envelope_parser() {
        for (case, response_body, expected_text) in non_streaming_response_cases() {
            let (provider, captured, server) = mock_non_streaming_response(response_body).await;
            let result = provider
                .chat_with_system(None, "hello", "test-model", None)
                .await;
            server.abort();

            let requests = captured.lock().unwrap();
            assert_eq!(requests.len(), 1, "{case}: expected one request");
            assert!(
                requests[0].get("tools").is_none(),
                "{case}: ordinary request unexpectedly contained tools: {}",
                requests[0]
            );

            match expected_text {
                Some(expected) => assert_eq!(
                    result.unwrap_or_else(|error| panic!("{case}: request failed: {error}")),
                    expected,
                    "{case}: response text mismatch"
                ),
                None => assert_sanitized_envelope_error(
                    &result.expect_err("malformed response must fail"),
                    case,
                ),
            }
        }
    }

    #[tokio::test]
    async fn native_tool_non_streaming_path_uses_shared_envelope_parser() {
        for (case, response_body, expected_text) in non_streaming_response_cases() {
            let (provider, captured, server) = mock_non_streaming_response(response_body).await;
            let messages = vec![ChatMessage::user("hello")];
            let tools = vec![zeroclaw_api::tool::ToolSpec::new(
                "echo",
                "Echo a value",
                serde_json::json!({
                    "type": "object",
                    "properties": {"value": {"type": "string"}}
                }),
            )];
            let result = provider
                .chat(
                    ProviderChatRequest {
                        messages: &messages,
                        tools: Some(&tools),
                        thinking: None,
                    },
                    "test-model",
                    None,
                )
                .await;
            server.abort();

            let requests = captured.lock().unwrap();
            assert_eq!(requests.len(), 1, "{case}: expected one request");
            assert_eq!(
                requests[0]["tools"][0]["function"]["name"], "echo",
                "{case}: request did not exercise the native-tool path: {}",
                requests[0]
            );

            match expected_text {
                Some(expected) => assert_eq!(
                    result
                        .unwrap_or_else(|error| panic!("{case}: request failed: {error}"))
                        .text
                        .as_deref(),
                    Some(expected),
                    "{case}: response text mismatch"
                ),
                None => assert_sanitized_envelope_error(
                    &result.expect_err("malformed response must fail"),
                    case,
                ),
            }
        }
    }

    #[test]
    fn x_api_key_auth_style() {
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("moonshot")
            .base_url("https://api.moonshot.cn")
            .credential(Some("ms-key"))
            .auth_style(AuthStyle::XApiKey)
            .build();
        assert!(matches!(p.auth_header, AuthStyle::XApiKey));
    }

    #[test]
    fn custom_auth_style() {
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("custom")
            .base_url("https://api.example.com")
            .credential(Some("key"))
            .auth_style(AuthStyle::Custom("X-Custom-Key".into()))
            .build();
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
        assert!(zhipu_jwt_bearer(".secret").is_err());
        assert!(zhipu_jwt_bearer("id.").is_err());
    }

    fn opencode_provider(base_url: &str) -> OpenAiCompatibleModelProvider {
        OpenAiCompatibleModelProvider::builder("opencode")
            .display_name("OpenCode Zen")
            .base_url(base_url)
            .credential(Some("test-key"))
            .auth_style(AuthStyle::Bearer)
            .build()
    }

    /// Request as the chat paths build it, addressed to the real endpoint,
    /// without sending anything.
    fn built_opencode_request(provider: &OpenAiCompatibleModelProvider) -> reqwest::Request {
        provider
            .apply_opencode_session_header(
                reqwest::Client::new().post(provider.chat_completions_url()),
            )
            .build()
            .expect("request must build")
    }

    /// Header value as it would go on the wire, without sending anything.
    fn built_session_header(provider: &OpenAiCompatibleModelProvider) -> Option<String> {
        built_opencode_request(provider)
            .headers()
            .get(OPENCODE_SESSION_HEADER)
            .map(|value| value.to_str().expect("header must be ASCII").to_string())
    }

    #[test]
    fn opencode_session_header_follows_the_built_request_destination() {
        // Header selection must agree with the parser that addresses the
        // request, not with a textual reading of the configured URI.
        for (base_url, api_path, expected_host) in [
            // `\` ends the authority; `@opencode.ai/v1` is only path.
            (
                "https://relay.example\\@opencode.ai/v1",
                None,
                "relay.example",
            ),
            // A percent-encoded host decodes to the relay.
            ("https://%6fpencode.ai/v1", None, "opencode.ai"),
            // `api_path` is appended to the base, so the base alone need not
            // name the destination; only the finished endpoint does.
            (
                "https:",
                Some("//opencode.ai/zen/v1/chat/completions"),
                "opencode.ai",
            ),
            (
                "https:",
                Some("//relay.example/v1/chat/completions"),
                "relay.example",
            ),
        ] {
            let provider = OpenAiCompatibleModelProvider::builder("opencode")
                .display_name("OpenCode Zen")
                .base_url(base_url)
                .api_path(api_path.map(str::to_string))
                .credential(Some("test-key"))
                .auth_style(AuthStyle::Bearer)
                .build();
            let request = built_opencode_request(&provider);
            let host = request.url().host_str().expect("request must have a host");
            assert_eq!(host, expected_host, "{base_url} + {api_path:?}");
            assert_eq!(
                request.headers().contains_key(OPENCODE_SESSION_HEADER),
                host == "opencode.ai",
                "{base_url} + {api_path:?}: header selection must match the request host {host}"
            );
        }
    }

    #[test]
    fn opencode_requests_carry_the_session_header() {
        for base_url in [
            "https://opencode.ai/zen/v1",
            "https://opencode.ai/zen/go/v1",
        ] {
            let header = built_session_header(&opencode_provider(base_url))
                .unwrap_or_else(|| panic!("{base_url} must carry the affinity header"));
            assert_eq!(header.len(), 32, "expected a 128-bit hex token");
            assert!(header.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn non_opencode_requests_do_not_carry_the_session_header() {
        assert!(
            built_session_header(&opencode_provider("https://api.openai.com/v1")).is_none(),
            "the header must not leak to unrelated providers"
        );
    }

    #[test]
    fn operator_pinned_session_header_is_not_overridden() {
        // `extra_headers` become client default headers, so emitting our own
        // value too would put the header on the wire twice.
        let mut headers = std::collections::HashMap::new();
        headers.insert(
            "X-Opencode-Session".to_string(),
            "pinned-by-operator".to_string(),
        );
        let provider = OpenAiCompatibleModelProvider::builder("opencode")
            .display_name("OpenCode Zen")
            .base_url("https://opencode.ai/zen/v1")
            .credential(Some("test-key"))
            .auth_style(AuthStyle::Bearer)
            .extra_headers(headers)
            .build();

        assert!(
            provider.opencode_session_value().is_none(),
            "an operator-pinned header must win over the derived value"
        );
    }

    #[test]
    fn malformed_pinned_session_header_falls_back_to_the_derived_token() {
        // The client builder skips a header value it cannot encode, so treating
        // it as a pin would leave the request with no affinity header at all.
        let headers = std::collections::HashMap::from([(
            "x-opencode-session".to_string(),
            "bad\nvalue".to_string(),
        )]);
        let provider = OpenAiCompatibleModelProvider::builder("opencode")
            .display_name("OpenCode Zen")
            .base_url("https://opencode.ai/zen/v1")
            .credential(Some("test-key"))
            .auth_style(AuthStyle::Bearer)
            .extra_headers(headers)
            .build();

        assert!(
            built_session_header(&provider).is_some(),
            "an invalid pinned value must not suppress the derived token"
        );
    }

    #[test]
    fn opencode_clients_carry_the_cross_host_redirect_policy() {
        // reqwest strips only credential headers on a cross-host redirect, so
        // every client an OpenCode provider builds must stop there instead.
        // reqwest's `Debug` names the redirect policy only when it is not the
        // default.
        let has_policy = |client: Client| format!("{client:?}").contains("redirect_policy");

        let opencode = opencode_provider("https://opencode.ai/zen/v1");
        assert!(has_policy(opencode.http_client()));
        assert!(has_policy(opencode.streaming_http_client()));

        let other = opencode_provider("https://api.openai.com/v1");
        assert!(!has_policy(other.http_client()));
        assert!(!has_policy(other.streaming_http_client()));
    }

    #[test]
    fn zhipu_jwt_auth_style_applies_correctly() {
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("Z.AI")
            .base_url("https://api.z.ai/api/coding/paas/v4")
            .credential(Some("testid.testsecret"))
            .auth_style(AuthStyle::ZhipuJwt)
            .build();
        assert!(matches!(p.auth_header, AuthStyle::ZhipuJwt));
    }

    /// Every request that reached the test server, as `(method, path)`.
    type WireLog = std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>;

    /// A server that records anything that arrives, on any method and path,
    /// and answers a well-formed chat completion. Recording everything is the
    /// point: a test asserting the log is empty then fails if a request
    /// escapes by a route the test did not anticipate, instead of passing
    /// because the request 404'd.
    async fn recording_server() -> (String, WireLog, tokio::task::JoinHandle<()>) {
        use axum::Router;
        use tokio::net::TcpListener;

        let log: WireLog = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log_for_route = std::sync::Arc::clone(&log);
        let app =
            Router::new().fallback(move |method: axum::http::Method, uri: axum::http::Uri| {
                let log = std::sync::Arc::clone(&log_for_route);
                async move {
                    log.lock()
                        .expect("wire log poisoned")
                        .push((method.to_string(), uri.path().to_string()));
                    axum::Json(serde_json::json!({
                        "choices": [{"message": {"content": "ok"}}],
                        "data": []
                    }))
                }
            });

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), log, server)
    }

    /// A `ZhipuJwt` credential that is not `id.secret` cannot be minted into a
    /// per-request token, and this provider fails closed rather than falling
    /// back to sending the stored value as a plain bearer token.
    ///
    /// Driven through every entry point on this provider that carries a
    /// credential, because the fallback lived in the helper they all share.
    /// Two properties per entry point: the operator gets a local error naming
    /// the provider, and — the security property — nothing reaches the server,
    /// so the stored value cannot have left the client in any form.
    ///
    /// The local error is also the behaviour change. These paths previously
    /// sent `Authorization: Bearer <stored value>` and surfaced whatever
    /// rejection the provider chose to return, which reads as a credential
    /// problem at the far end rather than a malformed credential here.
    #[tokio::test]
    async fn malformed_zhipu_credential_fails_closed_on_every_credential_bearing_path() {
        const MALFORMED: &str = "no-dot-separator-here";

        let (base_url, wire, server) = recording_server().await;
        let provider = OpenAiCompatibleModelProvider::builder("zai")
            .display_name("Z.AI")
            .base_url(&base_url)
            .credential(Some(MALFORMED))
            .auth_style(AuthStyle::ZhipuJwt)
            .build();

        let history = [ChatMessage::user("hi")];
        let stream_options = StreamOptions {
            enabled: true,
            count_tokens: false,
        };

        let mut refusals: Vec<(&str, String)> = Vec::new();
        let mut push = |entry_point: &'static str, err: String| refusals.push((entry_point, err));

        push(
            "list_models",
            provider.list_models().await.unwrap_err().to_string(),
        );
        push(
            "list_models_with_pricing",
            provider
                .list_models_with_pricing()
                .await
                .unwrap_err()
                .to_string(),
        );
        push(
            "chat_with_system",
            provider
                .chat_with_system(None, "hi", "m", None)
                .await
                .unwrap_err()
                .to_string(),
        );
        push(
            "chat_with_history",
            provider
                .chat_with_history(&history, "m", None)
                .await
                .unwrap_err()
                .to_string(),
        );
        push(
            "chat_with_tools",
            provider
                .chat_with_tools(&history, &[], "m", None)
                .await
                .unwrap_err()
                .to_string(),
        );
        push(
            "chat",
            provider
                .chat(
                    ProviderChatRequest {
                        messages: &history,
                        tools: None,
                        thinking: None,
                    },
                    "m",
                    None,
                )
                .await
                .unwrap_err()
                .to_string(),
        );
        push("warmup", provider.warmup().await.unwrap_err().to_string());

        // The streaming paths build their request inside a spawned task, so the
        // refusal arrives as the stream's first item rather than as a return
        // value. It must still be the first item — not an empty stream that
        // looks like a successful, contentless response.
        let mut events = provider.stream_chat(
            ProviderChatRequest {
                messages: &history,
                tools: None,
                thinking: None,
            },
            "m",
            None,
            stream_options,
        );
        push(
            "stream_chat",
            match events.next().await {
                Some(Err(err)) => err.to_string(),
                other => panic!("stream_chat must yield a refusal first, got {other:?}"),
            },
        );
        drop(events);

        let mut chunks = provider.stream_chat_with_system(None, "hi", "m", None, stream_options);
        push(
            "stream_chat_with_system",
            match chunks.next().await {
                Some(Err(err)) => err.to_string(),
                other => {
                    panic!("stream_chat_with_system must yield a refusal first, got {other:?}")
                }
            },
        );
        drop(chunks);

        let mut chunks = provider.stream_chat_with_history(&history, "m", None, stream_options);
        push(
            "stream_chat_with_history",
            match chunks.next().await {
                Some(Err(err)) => err.to_string(),
                other => {
                    panic!("stream_chat_with_history must yield a refusal first, got {other:?}")
                }
            },
        );
        drop(chunks);

        for (entry_point, err) in &refusals {
            assert!(
                err.contains("Z.AI"),
                "{entry_point}: the error must name the provider: {err}"
            );
            assert!(
                err.contains("no request was sent"),
                "{entry_point}: the error must say the request was refused locally: {err}"
            );
            assert!(
                !err.contains(MALFORMED),
                "{entry_point}: the error must not quote the stored credential: {err}"
            );
        }

        let seen = wire.lock().expect("wire log poisoned").clone();
        assert!(
            seen.is_empty(),
            "no request may leave the client for a credential that cannot be minted: {seen:?}"
        );
        server.abort();
    }

    /// The counterpart: a well-formed `id.secret` still reaches the provider,
    /// so failing closed did not disable Zhipu-auth families outright.
    #[tokio::test]
    async fn a_well_formed_zhipu_credential_still_reaches_the_provider() {
        let (base_url, wire, server) = recording_server().await;
        let provider = OpenAiCompatibleModelProvider::builder("zai")
            .display_name("Z.AI")
            .base_url(&base_url)
            .credential(Some("keyid.longlivedsecret"))
            .auth_style(AuthStyle::ZhipuJwt)
            .build();

        provider
            .chat_with_system(None, "hi", "m", None)
            .await
            .expect("a well-formed id.secret must still be usable");

        let seen = wire.lock().expect("wire log poisoned").clone();
        assert_eq!(
            seen.len(),
            1,
            "the well-formed case must still issue its request: {seen:?}"
        );
        server.abort();
    }

    /// A credential that cannot be minted refuses the request rather than
    /// building one. Sending it unauthenticated would leave the operator
    /// reading whatever 401 the provider returns instead of the local
    /// credential fault that actually happened.
    #[test]
    fn an_unmintable_zhipu_credential_refuses_the_request_instead_of_building_one() {
        const STORED: &str = "raw-secret-without-required-separator";

        let error = apply_auth_to_request(
            reqwest::Client::new().get("https://example.com"),
            &AuthStyle::ZhipuJwt,
            Some(STORED),
            "GLM",
        )
        .expect_err("an unmintable credential must not produce a request builder");

        let message = error.to_string();
        assert!(
            !message.contains(STORED),
            "the refusal must not quote the credential: {message}"
        );
        assert!(
            message.contains("GLM"),
            "the refusal must name the provider it belongs to: {message}"
        );
    }

    /// The three auth styles that pass a credential through untransformed keep
    /// doing so — fail-closed is scoped to the style that mints.
    #[test]
    fn untransformed_auth_styles_still_build_their_request() {
        for style in [
            AuthStyle::Bearer,
            AuthStyle::XApiKey,
            AuthStyle::Custom("X-Token".to_string()),
        ] {
            let request = apply_auth_to_request(
                reqwest::Client::new().get("https://example.com"),
                &style,
                Some("plain-key"),
                "test",
            )
            .expect("a credential needing no transformation always builds")
            .build()
            .expect("request should build");

            assert!(
                request
                    .headers()
                    .iter()
                    .any(|(_, v)| v.to_str().is_ok_and(|v| v.contains("plain-key"))),
                "expected the credential on the wire for {style:?}"
            );
        }
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
    // Custom endpoint path tests
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
    // ModelProvider-specific endpoint tests
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
            thinking_blocks: None,
        };

        let parsed = provider.parse_native_response(message);
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].id, "call_123");
        assert_eq!(parsed.tool_calls[0].name, "shell");
    }

    #[test]
    fn parse_native_response_rejects_non_object_tool_arguments() {
        let provider = make_model_provider("test", "https://example.com", None);
        let message = ResponseMessage {
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: Some("call_123".to_string()),
                kind: Some("function".to_string()),
                function: Some(Function {
                    name: Some("shell".to_string()),
                    arguments: Some(r#"["not", "an", "object"]"#.to_string()),
                }),
                name: None,
                arguments: None,
                parameters: None,
                extra_content: None,
            }]),
            reasoning_content: None,
            thinking_blocks: None,
        };

        let parsed = provider.parse_native_response(message);

        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].arguments, "{}");
    }

    #[tokio::test]
    async fn streaming_entry_points_sanitize_upstream_error_bodies() {
        use axum::{Router, http::StatusCode, response::IntoResponse, routing::post};
        use tokio::net::TcpListener;

        let secret = "sk-streaming-boundary-secret";
        let body = format!(r#"{{"error":"{secret} {}"}}"#, "x".repeat(4_000));
        let app = Router::new().route(
            "/chat/completions",
            post(move || {
                let body = body.clone();
                async move { (StatusCode::UNAUTHORIZED, body).into_response() }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let provider = make_model_provider("test", &format!("http://{addr}"), Some("key"));
        let options = StreamOptions {
            enabled: true,
            count_tokens: false,
        };

        let mut event_stream = provider.stream_chat(
            ProviderChatRequest {
                messages: &[ChatMessage::user("hi")],
                tools: None,
                thinking: None,
            },
            "test-model",
            None,
            options,
        );
        assert_sanitized_streaming_error(event_stream.next().await.unwrap().unwrap_err(), secret);

        let mut system_stream =
            provider.stream_chat_with_system(None, "hi", "test-model", None, options);
        assert_sanitized_streaming_error(system_stream.next().await.unwrap().unwrap_err(), secret);

        let mut history_stream = provider.stream_chat_with_history(
            &[ChatMessage::user("hi")],
            "test-model",
            None,
            options,
        );
        assert_sanitized_streaming_error(history_stream.next().await.unwrap().unwrap_err(), secret);

        server.abort();
    }

    fn assert_sanitized_streaming_error(error: StreamError, secret: &str) {
        let StreamError::ModelProvider(message) = error else {
            panic!("expected model-provider error, got {error}");
        };
        assert!(message.contains("401 Unauthorized"));
        assert!(message.contains("[REDACTED]"));
        assert!(!message.contains(secret));
        assert!(message.chars().count() <= 525);
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
            thinking_blocks: None,
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
            thinking_blocks: None,
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
            thinking_blocks: None,
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
            thinking_blocks: None,
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

    #[tokio::test]
    async fn operator_max_images_bounds_the_outbound_request() {
        // Behaviour boundary: the number of images that survive into the
        // messages this provider is about to send upstream. Asserting the
        // config field alone would still pass if the expansion pass kept
        // using library defaults.
        let temp = tempfile::tempdir().unwrap();
        // Minimal PNG signature bytes are enough for MIME detection.
        let png = [0x89u8, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        let first = temp.path().join("first.png");
        let second = temp.path().join("second.png");
        std::fs::write(&first, png).unwrap();
        std::fs::write(&second, png).unwrap();

        // One image per message: `trim_old_images` evicts whole messages, so
        // co-locating both in a single message would exercise that eviction
        // granularity rather than whether the operator's cap is honoured.
        let messages = vec![
            ChatMessage::user(format!("first [IMAGE:{}]", first.display())),
            ChatMessage::user(format!("second [IMAGE:{}]", second.display())),
        ];

        let build = |multimodal| {
            OpenAiCompatibleModelProvider::builder("test")
                .display_name("test")
                .base_url("https://example.com")
                .credential(None)
                .auth_style(AuthStyle::Bearer)
                .multimodal(multimodal)
                .build()
        };

        let permissive = build(zeroclaw_config::schema::MultimodalConfig::default());
        let prepared = permissive
            .normalize_messages_for_upstream(&messages)
            .await
            .expect("default policy prepares both images");
        assert_eq!(
            crate::multimodal::count_image_markers(&prepared),
            2,
            "default policy admits both images"
        );

        let capped = build(zeroclaw_config::schema::MultimodalConfig {
            max_images: 1,
            ..Default::default()
        });
        let prepared = capped
            .normalize_messages_for_upstream(&messages)
            .await
            .expect("capped policy still prepares the message");
        assert_eq!(
            crate::multimodal::count_image_markers(&prepared),
            1,
            "an operator capping max_images must bound the outbound request"
        );
    }

    #[test]
    fn builder_without_multimodal_falls_back_to_library_defaults() {
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .build();

        let defaults = zeroclaw_config::schema::MultimodalConfig::default();
        assert_eq!(provider.multimodal.max_images, defaults.max_images);
    }

    #[test]
    fn convert_messages_for_native_promotes_tool_result_image_markers() {
        // A tool result carrying an inline base64 image marker (e.g. a snapshot
        // tool) must serialize as structured `image_url` parts, not one large
        // text blob — vision backends count base64 bytes as text tokens and
        // reject the request as over-context otherwise
        let input = vec![ChatMessage::tool(
            r#"{"tool_call_id":"call_img","content":"snapshot captured\n\n[IMAGE:data:image/jpeg;base64,/9j/4AAQ]"}"#,
        )];

        let provider = make_model_provider("test", "https://example.com", None);
        assert_eq!(
            provider.tool_result_image_policy,
            ToolResultImagePolicy::ImageUrl
        );
        let converted = provider.convert_messages_for_native(&input, true);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].role, "tool");
        assert_eq!(converted[0].tool_call_id.as_deref(), Some("call_img"));

        let value = serde_json::to_value(
            converted[0]
                .content
                .as_ref()
                .expect("tool message should carry content"),
        )
        .unwrap();
        let parts = value
            .as_array()
            .expect("tool image content should serialize as a parts array");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "snapshot captured");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(
            parts[1]["image_url"]["url"],
            "data:image/jpeg;base64,/9j/4AAQ"
        );
    }

    #[test]
    fn convert_messages_for_native_omits_tool_result_image_payloads() {
        let input = vec![ChatMessage::tool(
            serde_json::json!({
                "tool_call_id": "call_img",
                "content": "before [IMAGE:data:image/jpeg;base64,/9j/4AAQ] middle [IMAGE:https://example.com/secret.png] after"
            })
            .to_string(),
        )];

        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .tool_result_image_policy(ToolResultImagePolicy::Omit)
            .build();
        let converted = provider.convert_messages_for_native(&input, false);
        let content = serde_json::to_value(
            converted[0]
                .content
                .as_ref()
                .expect("tool message should carry content"),
        )
        .unwrap();
        let content = content.as_str().expect("omitted tool content is text");

        assert_eq!(
            content,
            "before  middle  after\n\n[tool-result image omitted by provider policy]"
        );
        assert_eq!(
            content
                .matches("[tool-result image omitted by provider policy]")
                .count(),
            1
        );
        assert!(!content.contains("data:image"));
        assert!(!content.contains("https://example.com/secret.png"));
        assert!(!content.contains("/9j/4AAQ"));
        assert_eq!(converted[0].tool_call_id.as_deref(), Some("call_img"));
    }

    #[test]
    fn convert_messages_for_native_sanitizes_malformed_tool_result_json() {
        let input = vec![ChatMessage::tool(
            "malformed result [IMAGE:/tmp/secret.png]",
        )];
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .tool_result_image_policy(ToolResultImagePolicy::Omit)
            .build();

        let converted = provider.convert_messages_for_native(&input, true);
        let content = serde_json::to_value(
            converted[0]
                .content
                .as_ref()
                .expect("malformed tool message should carry content"),
        )
        .unwrap();
        let content = content.as_str().expect("sanitized fallback should be text");

        assert_eq!(converted[0].role, "tool");
        assert_eq!(
            content,
            "malformed result \n\n[tool-result image omitted by provider policy]"
        );
        assert_eq!(converted[0].tool_call_id, None);
        assert_eq!(converted[0].name, None);
        assert!(!content.contains("[IMAGE:"));
        assert!(!content.contains("/tmp/secret.png"));
    }

    #[test]
    fn convert_messages_for_native_sanitizes_non_string_tool_result_content() {
        let input = vec![ChatMessage::tool(
            serde_json::json!({
                "tool_call_id": "call_obj",
                "name": "read",
                "content": {"payload": "[IMAGE:/tmp/secret.png]"}
            })
            .to_string(),
        )];
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .tool_result_image_policy(ToolResultImagePolicy::Omit)
            .build();

        let converted = provider.convert_messages_for_native(&input, true);
        let content = serde_json::to_value(
            converted[0]
                .content
                .as_ref()
                .expect("non-string tool message should carry content"),
        )
        .unwrap();
        let content = content.as_str().expect("sanitized fallback should be text");

        assert_eq!(converted[0].tool_call_id.as_deref(), Some("call_obj"));
        assert_eq!(converted[0].name.as_deref(), Some("read"));
        assert!(content.contains("\"payload\":\""));
        assert!(content.contains(TOOL_RESULT_IMAGE_OMITTED_NOTICE));
        assert_eq!(content.matches(TOOL_RESULT_IMAGE_OMITTED_NOTICE).count(), 1);
        assert!(!content.contains("[IMAGE:"));
        assert!(!content.contains("/tmp/secret.png"));
    }

    #[test]
    fn convert_messages_for_native_sanitizes_unterminated_tool_result_marker() {
        let input = vec![ChatMessage::tool(
            serde_json::json!({
                "tool_call_id": "call_unterminated",
                "content": "prefix [IMAGE:/tmp/secret.png"
            })
            .to_string(),
        )];
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .tool_result_image_policy(ToolResultImagePolicy::Omit)
            .build();

        let converted = provider.convert_messages_for_native(&input, true);
        let content = serde_json::to_value(
            converted[0]
                .content
                .as_ref()
                .expect("unterminated tool message should carry content"),
        )
        .unwrap();
        let content = content
            .as_str()
            .expect("sanitized tool content should be text");

        assert_eq!(
            content,
            "prefix \n\n[tool-result image omitted by provider policy]"
        );
        assert_eq!(
            converted[0].tool_call_id.as_deref(),
            Some("call_unterminated")
        );
        assert!(!content.contains("[IMAGE:"));
        assert!(!content.contains("/tmp/secret.png"));
    }

    #[tokio::test]
    async fn chat_with_history_no_tools_sanitizes_tool_result_request_content() {
        let (mut provider, captured, server) = mock_non_streaming_response(serde_json::json!({
            "choices": [{"message": {"content": "ok"}}]
        }))
        .await;
        provider.tool_result_image_policy = ToolResultImagePolicy::Omit;

        let messages = vec![ChatMessage::tool(
            "history [IMAGE:data:image/png;base64,SECRET] tail",
        )];
        let response = provider
            .chat_with_history(&messages, "test-model", None)
            .await
            .expect("chat history should succeed");
        assert_eq!(response, "ok");

        let request = captured
            .lock()
            .expect("capture lock poisoned")
            .pop()
            .expect("server should capture request");
        let content = request["messages"][0]["content"]
            .as_str()
            .expect("tool content should serialize as a string");
        assert_eq!(
            content,
            "history  tail\n\n[tool-result image omitted by provider policy]"
        );
        assert!(!content.contains("[IMAGE:"));
        assert!(!content.contains("data:image"));
        assert!(!content.contains("SECRET"));
        server.abort();
    }

    #[tokio::test]
    async fn chat_with_history_no_tools_sanitizes_escaped_tool_result_marker() {
        let (mut provider, captured, server) = mock_non_streaming_response(serde_json::json!({
            "choices": [{"message": {"content": "ok"}}]
        }))
        .await;
        provider.tool_result_image_policy = ToolResultImagePolicy::Omit;

        let messages = vec![ChatMessage::tool(
            r#"{"tool_call_id":"call_escaped","name":"inspect","content":"before \u005bIMAGE:data:image/png;base64,SECRET] after"}"#,
        )];
        provider
            .chat_with_history(&messages, "test-model", None)
            .await
            .expect("chat history should succeed");

        let request = captured
            .lock()
            .expect("capture lock poisoned")
            .pop()
            .expect("server should capture request");
        let envelope: serde_json::Value = serde_json::from_str(
            request["messages"][0]["content"]
                .as_str()
                .expect("tool envelope should serialize as a string"),
        )
        .expect("tool envelope remains valid JSON");

        assert_eq!(envelope["tool_call_id"], "call_escaped");
        assert_eq!(envelope["name"], "inspect");
        assert_eq!(
            envelope["content"],
            "before  after\n\n[tool-result image omitted by provider policy]"
        );
        let serialized = envelope.to_string();
        assert!(!serialized.contains("[IMAGE:"));
        assert!(!serialized.contains("data:image"));
        assert!(!serialized.contains("SECRET"));
        server.abort();
    }

    #[test]
    fn convert_messages_for_native_omits_older_tool_results_across_rounds() {
        let messages = vec![
            ChatMessage::assistant(
                serde_json::json!({
                    "content": "",
                    "tool_calls": [{
                        "id": "call_old",
                        "name": "first",
                        "arguments": "{}"
                    }]
                })
                .to_string(),
            ),
            ChatMessage::tool(
                serde_json::json!({
                    "tool_call_id": "call_old",
                    "content": "old result [IMAGE:/tmp/old.png]"
                })
                .to_string(),
            ),
            ChatMessage::assistant(
                serde_json::json!({
                    "content": "",
                    "tool_calls": [{
                        "id": "call_new",
                        "name": "second",
                        "arguments": "{}"
                    }]
                })
                .to_string(),
            ),
            ChatMessage::tool(
                serde_json::json!({
                    "tool_call_id": "call_new",
                    "content": "new result [IMAGE:data:image/png;base64,NEW]"
                })
                .to_string(),
            ),
        ];

        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .tool_result_image_policy(ToolResultImagePolicy::Omit)
            .build();
        let converted = provider.convert_messages_for_native(&messages, true);

        assert_eq!(converted.len(), 4);
        for (index, expected_id) in [(1, "call_old"), (3, "call_new")] {
            assert_eq!(converted[index].role, "tool");
            assert_eq!(converted[index].tool_call_id.as_deref(), Some(expected_id));
            assert_eq!(
                converted[index].name.as_deref(),
                Some(if index == 1 { "first" } else { "second" })
            );
            let content = serde_json::to_value(
                converted[index]
                    .content
                    .as_ref()
                    .expect("historical tool result should carry content"),
            )
            .unwrap();
            let content = content.as_str().expect("omitted tool content is text");
            assert!(content.ends_with("[tool-result image omitted by provider policy]"));
            assert!(!content.contains("[IMAGE:"));
            assert!(!content.contains("data:image"));
            assert!(!content.contains("/tmp/old.png"));
            assert!(!content.contains("base64,NEW"));
        }
    }

    #[test]
    fn convert_messages_for_native_keeps_direct_user_images_under_omit_policy() {
        let input = vec![ChatMessage::user(
            "describe this [IMAGE:data:image/png;base64,USER]",
        )];
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .tool_result_image_policy(ToolResultImagePolicy::Omit)
            .build();

        let converted = provider.convert_messages_for_native(&input, true);
        let content = serde_json::to_value(
            converted[0]
                .content
                .as_ref()
                .expect("user message should carry content"),
        )
        .unwrap();
        let parts = content
            .as_array()
            .expect("direct user image should remain structured");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,USER");
    }

    #[test]
    fn convert_messages_for_native_tool_result_resolves_name_from_tool_name_map() {
        let history_json = serde_json::json!({
            "content": "",
            "tool_calls": [{
                "id": "call_abc",
                "name": "shell",
                "arguments": "{\"cmd\":\"pwd\"}"
            }]
        });
        let messages = vec![
            ChatMessage::assistant(history_json.to_string()),
            ChatMessage::tool(
                serde_json::json!({
                    "tool_call_id": "call_abc",
                    "content": "done"
                })
                .to_string(),
            ),
        ];

        let provider = make_model_provider("test", "https://example.com", None);
        let native = provider.convert_messages_for_native(&messages, true);
        assert_eq!(native.len(), 2);
        assert_eq!(native[0].role, "assistant");
        let tool_msg = &native[1];
        assert_eq!(tool_msg.role, "tool");
        assert_eq!(
            tool_msg.name.as_deref(),
            Some("shell"),
            "tool name should resolve from paired assistant tool-call"
        );
    }

    #[test]
    fn convert_messages_for_native_keeps_tool_result_image_markers_as_text_when_disabled() {
        // Models that don't accept structured image parts (the same gate that
        // keeps user image markers as text) must keep tool-result markers
        // verbatim — preserving prior behavior and thesafety posture.
        let input = vec![ChatMessage::tool(
            r#"{"tool_call_id":"call_img","content":"snapshot captured\n\n[IMAGE:data:image/jpeg;base64,/9j/4AAQ]"}"#,
        )];

        let provider = make_model_provider("test", "https://example.com", None);
        let converted = provider.convert_messages_for_native(&input, false);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].role, "tool");
        assert!(matches!(
            converted[0].content.as_ref(),
            Some(MessageContent::Text(value))
                if value == "snapshot captured\n\n[IMAGE:data:image/jpeg;base64,/9j/4AAQ]"
        ));
    }

    #[test]
    fn convert_messages_for_native_tool_result_falls_back_to_content_name() {
        // When there is no paired assistant tool-call, the tool message's
        // own "name" field should be used as a fallback.
        let messages = vec![ChatMessage::tool(
            serde_json::json!({
                "tool_call_id": "call_xyz",
                "name": "read",
                "content": "file contents"
            })
            .to_string(),
        )];

        let provider = make_model_provider("test", "https://example.com", None);
        let native = provider.convert_messages_for_native(&messages, true);
        assert_eq!(native.len(), 1);
        assert_eq!(native[0].role, "tool");
        assert_eq!(
            native[0].name.as_deref(),
            Some("read"),
            "tool name should fall back to the content name field"
        );
    }

    #[test]
    fn native_message_name_serialized_only_when_present() {
        // Role "tool" messages must include `name` when set; non-tool
        // messages and tool messages without a name must omit the key.
        let tool_with_name = NativeMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text("result".to_string())),
            tool_call_id: Some("call_1".to_string()),
            tool_calls: None,
            reasoning_content: None,
            reasoning: None,
            thinking_blocks: None,
            name: Some("shell".to_string()),
        };
        let json = serde_json::to_string(&tool_with_name).unwrap();
        assert!(
            json.contains("\"name\":\"shell\""),
            "name should be present when Some for tool messages"
        );

        let tool_without_name = NativeMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text("result".to_string())),
            tool_call_id: Some("call_2".to_string()),
            tool_calls: None,
            reasoning_content: None,
            reasoning: None,
            thinking_blocks: None,
            name: None,
        };
        let json = serde_json::to_string(&tool_without_name).unwrap();
        assert!(
            !json.contains("\"name\""),
            "name should be omitted when None"
        );

        let assistant_msg = NativeMessage {
            role: "assistant".to_string(),
            content: Some(MessageContent::Text("hello".to_string())),
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: None,
            reasoning: None,
            thinking_blocks: None,
            name: None,
        };
        let json = serde_json::to_string(&assistant_msg).unwrap();
        assert!(
            !json.contains("\"name\""),
            "name should be omitted for non-tool messages"
        );
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
            tools: Some(vec![NativeToolSpec {
                kind: "function".to_string(),
                extra: serde_json::Map::new(),
                function: NativeToolFunctionSpec {
                    extra: serde_json::Map::new(),
                    name: "shell".to_string(),
                    description: "Run a shell command".to_string(),
                    parameters: std::sync::Arc::new(serde_json::json!({"type": "object"})),
                },
            }]),
            tool_choice: Some("auto".to_string()),
            max_tokens: None,
            extra_body: None,
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

        let (output, system_merged) =
            OpenAiCompatibleModelProvider::flatten_system_messages(&input, true);
        assert!(system_merged, "merged into the existing user message");
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

        let (output, system_merged) =
            OpenAiCompatibleModelProvider::flatten_system_messages(&input, true);
        assert!(system_merged, "synthetic user inserted as the carrier");
        assert_eq!(output.len(), 2);
        assert_eq!(output[0].role, "user");
        assert_eq!(output[0].content, "core policy");
        assert_eq!(output[1].role, "assistant");
        assert_eq!(output[1].content, "ack");
    }

    #[test]
    fn effective_content_preserves_literal_think_tags() {
        // The deleted `strip_think_tags()` helper searched for the exact
        // substring `<think>` / `</think>` and stripped those blocks
        // unconditionally. This regression pins that literal `<think>` tags
        // now round-trip byte-for-byte, including legitimate uses where the
        // model legitimately discusses the tag (HTML sample, code quoting,
        // meta-discussion).
        let json = r#"{"choices":[{"message":{"content":"Here is the HTML: <think>internal note</think>"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(
            msg.effective_content(),
            "Here is the HTML: <think>internal note</think>"
        );
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
        let tools = vec![zeroclaw_api::tool::ToolSpec::new(
            "shell_exec",
            "Execute shell command",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" }
                },
                "required": ["command"]
            }),
        )];

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
        let model_provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .reasoning_effort(Some("high".to_string()))
            .build();

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
            model_provider.reasoning_effort_for_model("gpt-5-chat-latest"),
            None,
            "gpt-5*-chat-latest are non-reasoning chat-router models and must not receive reasoning_effort",
        );
        assert_eq!(
            model_provider.reasoning_effort_for_model("gpt-5.1-chat-latest"),
            None,
            "gpt-5*-chat-latest are non-reasoning chat-router models and must not receive reasoning_effort",
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
    async fn warmup_uses_models_endpoint_with_existing_auth() {
        use axum::{
            Router,
            body::Body,
            http::{Request, StatusCode},
        };
        use tokio::net::TcpListener;

        for (base_path, status, auth_style, credential, header) in [
            (
                "",
                200,
                AuthStyle::Bearer,
                Some("test-key"),
                Some(("authorization", "Bearer test-key")),
            ),
            (
                "/v1",
                401,
                AuthStyle::Bearer,
                Some("test-key"),
                Some(("authorization", "Bearer test-key")),
            ),
            (
                "/v1/",
                404,
                AuthStyle::XApiKey,
                Some("test-key"),
                Some(("x-api-key", "test-key")),
            ),
            (
                "/custom/api",
                405,
                AuthStyle::Custom("x-provider-key".into()),
                Some("test-key"),
                Some(("x-provider-key", "test-key")),
            ),
            ("/v1", 500, AuthStyle::Bearer, None, None),
        ] {
            let (tx, mut rx) = tokio::sync::mpsc::channel(1);
            let app = Router::new().fallback(move |request: Request<Body>| {
                let tx = tx.clone();
                async move {
                    tx.send((
                        request.method().clone(),
                        request.uri().path().to_owned(),
                        request.headers().clone(),
                    ))
                    .await
                    .unwrap();
                    (StatusCode::from_u16(status).unwrap(), "probe body")
                }
            });
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = ::zeroclaw_spawn::spawn!(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let provider = OpenAiCompatibleModelProvider::builder("test")
                .display_name("test")
                .base_url(&format!("http://{addr}{base_path}"))
                .credential(credential)
                .auth_style(auth_style)
                .extra_headers(std::collections::HashMap::from([(
                    "x-probe-test".into(),
                    "preserved".into(),
                )]))
                .api_path((status == 500).then(|| "/inference".to_string()))
                .build();
            let result = provider.warmup().await;
            server.abort();
            assert!(
                result.is_ok(),
                "HTTP {status} must not fail warmup: {result:?}"
            );
            let (method, path, headers) = rx.recv().await.unwrap();
            assert_eq!(method, "GET");
            assert_eq!(headers.get("x-probe-test").unwrap(), "preserved");
            assert_eq!(path, format!("{}/models", base_path.trim_end_matches('/')));
            if let Some((name, value)) = header {
                assert_eq!(headers.get(name).unwrap(), value);
            } else {
                assert!(!headers.contains_key("authorization"));
            }
        }
    }

    #[tokio::test]
    async fn warmup_drains_response_with_configured_timeout() {
        use axum::{
            Router,
            body::{Body, Bytes},
        };
        let app = Router::new().fallback(|| async {
            Body::from_stream(futures_util::stream::once(async {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                Ok::<_, std::io::Error>(Bytes::from_static(b"catalog"))
            }))
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url(&format!("http://{addr}"))
            .auth_style(AuthStyle::Bearer)
            .timeout_secs(1)
            .build();
        let result = provider.warmup().await;
        server.abort();
        assert!(
            result
                .unwrap_err()
                .downcast_ref::<reqwest::Error>()
                .unwrap()
                .is_timeout()
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
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("Qwen")
            .base_url("https://dashscope.aliyuncs.com/compatible-mode/v1")
            .credential(Some("k"))
            .auth_style(AuthStyle::Bearer)
            .vision(true)
            .build();
        let caps = <OpenAiCompatibleModelProvider as ModelProvider>::capabilities(&p);
        assert!(caps.native_tool_calling);
        assert!(caps.vision);
    }

    #[test]
    fn minimax_provider_supports_native_tool_calling_with_system_merge() {
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("MiniMax")
            .base_url("https://api.minimax.chat/v1")
            .credential(Some("k"))
            .auth_style(AuthStyle::Bearer)
            .merge_system_into_user_preserving_native()
            .build();
        let caps = <OpenAiCompatibleModelProvider as ModelProvider>::capabilities(&p);
        assert!(
            caps.native_tool_calling,
            "MiniMax should preserve native tool calling when system messages are merged"
        );
        assert!(!caps.vision);
    }

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
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("MiniMax")
            .base_url("https://api.minimax.chat/v1")
            .credential(Some("k"))
            .auth_style(AuthStyle::Bearer)
            .merge_system_into_user()
            .build();
        let stripped = p.strip_native_tool_messages(&messages);
        // tool message dropped; the pre-tool narration and the reply that
        // follows the tool result are now coalesced into a single assistant
        // message so the output never contains consecutive assistants.
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
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("MiniMax")
            .base_url("https://api.minimax.chat/v1")
            .credential(Some("k"))
            .auth_style(AuthStyle::Bearer)
            .merge_system_into_user()
            .build();
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
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("MiniMax")
            .base_url("https://api.minimax.chat/v1")
            .credential(Some("k"))
            .auth_style(AuthStyle::Bearer)
            .merge_system_into_user()
            .build();
        let stripped = p.strip_native_tool_messages(&messages);
        assert_eq!(stripped.len(), 4);
        for (orig, result) in messages.iter().zip(stripped.iter()) {
            assert_eq!(orig.role, result.role);
            assert_eq!(orig.content, result.content);
        }
    }

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
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("NativeToolProvider")
            .base_url("https://api.example.com/v1")
            .credential(Some("k"))
            .auth_style(AuthStyle::Bearer)
            .build();
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
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("TestProvider")
            .base_url("https://example.com")
            .credential(Some("k"))
            .auth_style(AuthStyle::Bearer)
            .user_agent("zeroclaw-test/1.0")
            .build();
        let caps = <OpenAiCompatibleModelProvider as ModelProvider>::capabilities(&p);
        assert!(caps.native_tool_calling);
        assert!(!caps.vision);
        assert_eq!(p.user_agent.as_deref(), Some("zeroclaw-test/1.0"));
    }

    #[test]
    fn user_agent_and_vision_constructor_preserves_capability_flags() {
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("VisionModelProvider")
            .base_url("https://example.com")
            .credential(Some("k"))
            .auth_style(AuthStyle::Bearer)
            .user_agent("zeroclaw-test/vision")
            .vision(true)
            .build();
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
        // bare local paths inside `[IMAGE:...]` markers
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

        let mut provider = make_model_provider("test", "https://example.com", None);
        provider.tool_result_image_policy = ToolResultImagePolicy::Omit;
        let normalized = provider
            .normalize_messages_for_upstream(std::slice::from_ref(&msg))
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

    #[tokio::test]
    async fn normalize_messages_for_upstream_omit_preserves_tool_envelope() {
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .tool_result_image_policy(ToolResultImagePolicy::Omit)
            .build();
        let message = ChatMessage::tool(
            r#"{"tool_call_id":"call_image","name":"inspect","content":"before \u005bIMAGE:/tmp/secret.png] after"}"#,
        );

        let normalized = provider
            .normalize_messages_for_upstream(std::slice::from_ref(&message))
            .await
            .expect("normalize ok");
        let envelope: serde_json::Value =
            serde_json::from_str(&normalized[0].content).expect("tool envelope remains valid JSON");

        assert_eq!(envelope["tool_call_id"], "call_image");
        assert_eq!(envelope["name"], "inspect");
        assert_eq!(
            envelope["content"],
            "before  after\n\n[tool-result image omitted by provider policy]"
        );
        assert!(!normalized[0].content.contains("[IMAGE:"));
        assert!(!normalized[0].content.contains("/tmp/secret.png"));
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
                thinking_blocks: None,
            }],
            temperature: Some(0.7),
            stream: Some(false),
            stream_options: None,
            reasoning_effort: None,
            tool_stream: None,
            tools: Some(tools),
            tool_choice: Some("auto".to_string()),
            max_tokens: None,
            extra_body: None,
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
                thinking_blocks: None,
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
            extra_body: None,
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
                thinking_blocks: None,
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
            extra_body: None,
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
        let tools = vec![NativeToolSpec {
            kind: "function".to_string(),
            extra: serde_json::Map::new(),
            function: NativeToolFunctionSpec {
                extra: serde_json::Map::new(),
                name: "shell".to_string(),
                description: "Run a shell command".to_string(),
                parameters: std::sync::Arc::new(serde_json::json!({})),
            },
        }];

        let request = p.build_native_tool_chat_request(
            &messages,
            Some(tools),
            "deepseek-v4-flash",
            Some(0.7),
            true,
            false,
            None,
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

        let (flattened, system_merged) =
            OpenAiCompatibleModelProvider::flatten_system_messages(&messages, true);
        assert!(system_merged);
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

        let (flattened, system_merged) =
            OpenAiCompatibleModelProvider::flatten_system_messages(&messages, false);
        assert!(!system_merged);
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

        let (flattened, system_merged) =
            OpenAiCompatibleModelProvider::flatten_system_messages(&messages, false);
        assert!(!system_merged, "empty system content merged nothing");

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

        let (flattened, system_merged) =
            OpenAiCompatibleModelProvider::flatten_system_messages(&messages, true);
        assert!(system_merged);
        assert_eq!(flattened.len(), 2);
        assert_eq!(flattened[0].role, "user");
        assert_eq!(flattened[0].content, "Synthetic system");
        assert_eq!(flattened[1].role, "assistant");
    }

    #[test]
    fn effective_content_preserves_unclosed_think_tag() {
        // An unclosed literal `<think>` tag must NOT discard the rest of the
        // response. The old `strip_think_tags()` helper saw no closing
        // `</think>` and dropped the trailing tail, collapsing
        // "Visible <think>hidden tail" to "Visible". The new path returns
        // the input unchanged.
        let json = r#"{"choices":[{"message":{"content":"Visible <think>hidden tail"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.effective_content(), "Visible <think>hidden tail");
    }

    #[test]
    fn effective_content_preserves_multiple_think_blocks() {
        // Multiple literal `<think>` blocks in `content` survive the removal
        // intact. The old `strip_think_tags()` helper would have collapsed
        // the visible text to "Answer A  and B  done" — the double spaces
        // mark where `<think>hidden 1</think>` and `<think>hidden 2</think>`
        // used to be — losing the inter-block separators and the tag
        // delimiters themselves.
        let json = r#"{"choices":[{"message":{"content":"Answer A <think>hidden 1</think> and B <think>hidden 2</think> done"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(
            msg.effective_content(),
            "Answer A <think>hidden 1</think> and B <think>hidden 2</think> done"
        );
    }
    #[test]
    fn effective_content_preserves_think_tags_with_reasoning_content() {
        // When both `content` and `reasoning_content` are present,
        // the literal `<think>` blocks in `content` survive intact while
        // `reasoning_content` is preserved separately and is NOT leaked
        // into the response text.
        let json = r#"{"choices":[{"message":{"content":"Visible <think>hidden tail</think>","reasoning_content":"reasoning separately"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(
            msg.effective_content(),
            "Visible <think>hidden tail</think>"
        );
        assert!(!msg.effective_content().contains("reasoning separately"));
        assert_eq!(
            msg.reasoning_content.as_deref(),
            Some("reasoning separately")
        );
    }

    // ----------------------------------------------------------
    // Reasoning model fallback tests (reasoning_content)
    // ----------------------------------------------------------

    #[test]
    fn reasoning_content_does_not_leak_when_content_empty() {
        // reasoning_content must NOT leak into effective_content —
        // it is preserved separately in ChatResponse.reasoning_content
        let json = r#"{"choices":[{"message":{"content":"","reasoning_content":"Thinking output here"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.effective_content(), "");
        assert_eq!(
            msg.reasoning_content.as_deref(),
            Some("Thinking output here")
        );
    }

    #[test]
    fn reasoning_content_does_not_leak_when_content_null() {
        let json =
            r#"{"choices":[{"message":{"content":null,"reasoning_content":"Fallback text"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.effective_content(), "");
        assert_eq!(msg.reasoning_content.as_deref(), Some("Fallback text"));
    }

    #[test]
    fn reasoning_content_does_not_leak_when_content_missing() {
        let json = r#"{"choices":[{"message":{"reasoning_content":"Only reasoning"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.effective_content(), "");
        assert_eq!(msg.reasoning_content.as_deref(), Some("Only reasoning"));
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
    fn reasoning_content_preserved_when_content_only_think_tags() {
        // The compatible provider no longer strips literal
        // `<think>...</think>` blocks from `content`. Previously the
        // `<think>secret</think>`-only content was collapsed to the empty
        // string by `strip_think_tags()`, and `effective_content()` returned
        // `""` so the visible-text field was effectively replaced by the
        // model's chain-of-thought marker. Now the literal `<think>` tags
        // round-trip into `effective_content()` byte-for-byte, and
        // `reasoning_content` is still preserved separately and not leaked
        // into the response text.
        let json = r#"{"choices":[{"message":{"content":"<think>secret</think>","reasoning_content":"Thinking text"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert!(msg.effective_content().contains("secret"));
        assert!(msg.effective_content().contains("<think>"));
        assert_eq!(
            msg.effective_content_optional().as_deref(),
            Some("<think>secret</think>"),
        );
        assert_eq!(msg.reasoning_content.as_deref(), Some("Thinking text"));
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

    // OpenRouter and vLLM (>= v0.16.0) emit reasoning
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
        // effective_content returns "" when content is None — reasoning
        // is preserved separately, not leaked into the response text.
        assert_eq!(msg.effective_content(), "");
    }

    #[test]
    fn response_message_canonical_reasoning_content_still_works() {
        // Existing providers continue to populate reasoning_content directly.
        let json = r#"{"content":null,"reasoning_content":"canonical thought","tool_calls":null}"#;
        let msg: ResponseMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.reasoning_content.as_deref(), Some("canonical thought"));
    }

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
    fn api_response_parses_openai_cached_tokens() {
        let json = r#"{
            "choices": [{"message": {"content": "Hello"}}],
            "usage": {
                "prompt_tokens": 150,
                "completion_tokens": 60,
                "prompt_tokens_details": {"cached_tokens": 120}
            }
        }"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage.unwrap().into_provider_usage();
        assert_eq!(usage.input_tokens, Some(150));
        assert_eq!(usage.output_tokens, Some(60));
        assert_eq!(usage.cached_input_tokens, Some(120));
    }

    #[test]
    fn api_response_parses_gateway_cache_creation_tokens() {
        // Translating gateways forward Anthropic's cache-write counter inside
        // `prompt_tokens_details` when providers include that usage detail.
        let json = r#"{
            "choices": [{"message": {"content": "Hello"}}],
            "usage": {
                "prompt_tokens": 150,
                "completion_tokens": 60,
                "prompt_tokens_details": {
                    "cached_tokens": 120,
                    "cache_creation_input_tokens": 25
                }
            }
        }"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage.unwrap().into_provider_usage();
        assert_eq!(usage.cached_input_tokens, Some(120));
        assert_eq!(usage.cache_creation_input_tokens, Some(25));
    }

    #[test]
    fn api_response_parses_deepseek_cached_tokens() {
        let json = r#"{
            "choices": [{"message": {"content": "Hello"}}],
            "usage": {
                "prompt_tokens": 150,
                "completion_tokens": 60,
                "prompt_cache_hit_tokens": 100,
                "prompt_tokens_details": {"cached_tokens": 80}
            }
        }"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage.unwrap().into_provider_usage();
        assert_eq!(usage.cached_input_tokens, Some(100));
    }

    #[test]
    fn api_response_parses_non_integer_cached_tokens_lossily() {
        let json = r#"{
            "choices": [{"message": {"content": "Hello"}}],
            "usage": {
                "prompt_tokens": 150,
                "completion_tokens": 60,
                "prompt_tokens_details": {"cached_tokens": "2.5e2"}
            }
        }"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage.unwrap().into_provider_usage();
        assert_eq!(usage.cached_input_tokens, Some(250));
    }

    #[test]
    fn api_response_ignores_invalid_cached_tokens_without_losing_usage() {
        let json = r#"{
            "choices": [{"message": {"content": "Hello"}}],
            "usage": {
                "prompt_tokens": 150,
                "completion_tokens": 60,
                "prompt_cache_hit_tokens": -1,
                "prompt_tokens_details": {"cached_tokens": "not-a-number"}
            }
        }"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage.unwrap().into_provider_usage();
        assert_eq!(usage.input_tokens, Some(150));
        assert_eq!(usage.output_tokens, Some(60));
        assert_eq!(usage.cached_input_tokens, None);
    }

    #[test]
    fn stream_chunk_parses_cached_tokens() {
        let json = r#"{
            "choices": [],
            "usage": {
                "prompt_tokens": 99,
                "completion_tokens": 11,
                "prompt_tokens_details": {"cached_tokens": 42}
            }
        }"#;
        let chunk: StreamChunkResponse = serde_json::from_str(json).unwrap();
        let usage = chunk.usage.unwrap().into_provider_usage();
        assert_eq!(usage.input_tokens, Some(99));
        assert_eq!(usage.output_tokens, Some(11));
        assert_eq!(usage.cached_input_tokens, Some(42));
    }

    #[test]
    fn stream_chunk_prefers_deepseek_prompt_cache_hit_tokens() {
        let json = r#"{
            "id":"14037a3e-81f7-4559-b9ae-161bcb17c34c",
            "object":"chat.completion.chunk",
            "created":1780971871,
            "model":"deepseek-v4-flash",
            "choices":[{"index":0,"delta":{"content":"","reasoning_content":null},"finish_reason":"tool_calls"}],
            "usage": {
                "prompt_tokens": 13313,
                "completion_tokens": 175,
                "total_tokens": 13488,
                "prompt_tokens_details": {"cached_tokens": 384},
                "completion_tokens_details": {"reasoning_tokens": 100},
                "prompt_cache_hit_tokens": 384,
                "prompt_cache_miss_tokens": 12929
            }
        }"#;
        let chunk: StreamChunkResponse = serde_json::from_str(json).unwrap();
        let usage = chunk.usage.unwrap().into_provider_usage();
        assert_eq!(usage.input_tokens, Some(13313));
        assert_eq!(usage.output_tokens, Some(175));
        assert_eq!(usage.cached_input_tokens, Some(384));
    }

    #[test]
    fn api_response_parses_anthropic_cache_read_tokens() {
        let json = r#"{
            "choices": [{"message": {"content": "Hello"}}],
            "usage": {
                "prompt_tokens": 4863,
                "completion_tokens": 0,
                "cache_read_input_tokens": 4802,
                "cache_creation_input_tokens": 0
            }
        }"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage.unwrap().into_provider_usage();
        assert_eq!(usage.input_tokens, Some(4863));
        assert_eq!(usage.cached_input_tokens, Some(4802));
    }

    /// All three cached-token shapes on one response: the Anthropic fields
    /// are upstream-reported and win over the gateway-accounted DeepSeek
    /// and OpenAI shapes, which keep their existing relative order.
    #[test]
    fn api_response_prefers_anthropic_cache_read_across_all_shapes() {
        let json = r#"{
            "choices": [{"message": {"content": "Hello"}}],
            "usage": {
                "prompt_tokens": 4863,
                "completion_tokens": 0,
                "cache_read_input_tokens": 4802,
                "prompt_cache_hit_tokens": 100,
                "prompt_tokens_details": {"cached_tokens": 80}
            }
        }"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage.unwrap().into_provider_usage();
        assert_eq!(usage.cached_input_tokens, Some(4802));
    }

    /// An upstream-reported zero is authoritative, not missing data: when
    /// the Anthropic shape says zero reads, the gateway-accounted OpenAI
    /// figure must NOT be substituted (that would double-count gateway-side
    /// accounting against the upstream's authoritative answer).
    #[test]
    fn api_response_anthropic_zero_read_blocks_openai_fallback() {
        let json = r#"{
            "choices": [{"message": {"content": "Hello"}}],
            "usage": {
                "prompt_tokens": 4863,
                "completion_tokens": 0,
                "cache_read_input_tokens": 0,
                "prompt_tokens_details": {"cached_tokens": 80}
            }
        }"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage.unwrap().into_provider_usage();
        assert_eq!(usage.cached_input_tokens, Some(0));
    }

    #[test]
    fn stream_chunk_parses_anthropic_cache_read_tokens() {
        let json = r#"{
            "choices": [],
            "usage": {
                "prompt_tokens": 99,
                "completion_tokens": 11,
                "cache_read_input_tokens": 42,
                "cache_creation_input_tokens": 0
            }
        }"#;
        let chunk: StreamChunkResponse = serde_json::from_str(json).unwrap();
        let usage = chunk.usage.unwrap().into_provider_usage();
        assert_eq!(usage.input_tokens, Some(99));
        assert_eq!(usage.output_tokens, Some(11));
        assert_eq!(usage.cached_input_tokens, Some(42));
    }

    /// Cache-write tokens are logged for write-premium visibility. The log
    /// must carry the counts, not the prompt content.
    #[test]
    fn cache_creation_tokens_are_logged_with_counts_only() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let json = r#"{
            "choices": [{"message": {"content": "Hello"}}],
            "usage": {
                "prompt_tokens": 4863,
                "completion_tokens": 0,
                "cache_creation_input_tokens": 4803
            }
        }"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage.unwrap().into_provider_usage();
        // Write-only response: upstream reported no cache read, so the
        // cached-token figure stays unset.
        assert_eq!(usage.cached_input_tokens, None);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let found = 'search: loop {
            while let Ok(event) = rx.try_recv() {
                let matches_message = event.get("message").and_then(|value| value.as_str())
                    == Some("gateway-reported Anthropic cache write (billed at the write premium)");
                let matches_counts = event
                    .get("attributes")
                    .and_then(|attributes| attributes.get("cache_creation_input_tokens"))
                    == Some(&serde_json::json!(4803));
                if matches_message && matches_counts && matches_source_file(&event) {
                    break 'search true;
                }
            }
            if std::time::Instant::now() >= deadline {
                break 'search false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(found, "cache-write log record with counts must be emitted");
    }

    fn matches_source_file(event: &serde_json::Value) -> bool {
        event
            .get("attributes")
            .and_then(|attributes| attributes.get("_file"))
            .and_then(|value| value.as_str())
            .is_some_and(|file| file.ends_with("compatible.rs"))
    }

    /// End to end on the tools path: a translated-gateway response carrying
    /// Anthropic-shaped usage populates `TokenUsage.cached_input_tokens`.
    /// Parsing is flag-independent (the gateway sends these fields on
    /// Anthropic-backed routes regardless of the client flag).
    #[tokio::test]
    async fn chat_with_tools_surfaces_anthropic_cache_read_tokens() {
        let (provider, _captured, server) = mock_non_streaming_response(serde_json::json!({
            "choices": [{"message": {"content": "ok"}}],
            "usage": {
                "prompt_tokens": 4863,
                "completion_tokens": 4,
                "cache_read_input_tokens": 4802,
                "cache_creation_input_tokens": 0
            }
        }))
        .await;
        let messages = vec![ChatMessage::user("hi")];
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get weather",
                "parameters": {"type": "object", "properties": {}}
            }
        })];
        let response = provider
            .chat_with_tools(&messages, &tools, "test-model", None)
            .await;
        server.abort();
        let response = response.unwrap_or_else(|error| panic!("tools request failed: {error}"));
        let usage = response.usage.expect("usage must be captured");
        assert_eq!(usage.cached_input_tokens, Some(4802));
        assert_eq!(usage.input_tokens, Some(4863));
    }

    /// Flag interaction: the thinking object reaches the wire through
    /// `extra_body` (the same surface the thinking-passthrough feature
    /// uses), and it must ride alongside the cache breakpoints in one
    /// request: neither injection disturbs the other. This proves generic
    /// flattened-body coexistence on the structured path; the flag-side
    /// composition proof lives in
    /// `cache_and_thinking_passthrough_flags_compose_in_one_request` below.
    #[tokio::test]
    async fn cache_breakpoints_coexist_with_thinking_object_in_one_request() {
        let (provider, captured, server) = mock_streaming_cache_capture(true).await;
        // Builder-level extra_body mirrors what provider_extra supplies for
        // thinking-capable gateways; the flag-side equivalent is proven by
        // cache_and_thinking_passthrough_flags_compose_in_one_request below.
        let provider = OpenAiCompatibleModelProvider {
            extra_body: Some(serde_json::json!({
                "thinking": {"type": "enabled", "budget_tokens": 2048},
            })),
            ..provider
        };
        let messages = vec![
            ChatMessage::system("you are brief"),
            ChatMessage::user("hi"),
            ChatMessage::assistant("hello"),
            ChatMessage::user("bye"),
        ];
        let result = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test-model",
                None,
            )
            .await;
        server.abort();
        result.unwrap_or_else(|error| panic!("combined request failed: {error}"));
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]["thinking"],
            serde_json::json!({"type": "enabled", "budget_tokens": 2048}),
            "the thinking object must ride at the top level untouched"
        );
        assert_eq!(
            requests[0]["messages"][0]["content"][0]["cache_control"],
            serde_json::json!({"type": "ephemeral"}),
            "system cache breakpoint present alongside the thinking object"
        );
        assert_eq!(
            requests[0]["messages"][3]["content"][0]["cache_control"],
            serde_json::json!({"type": "ephemeral"}),
            "rolling cache breakpoint present alongside the thinking object"
        );
    }

    /// Flag composition, the real thing: both operator flags on one
    /// provider, runtime thinking params supplied by the caller, and the
    /// cache breakpoints still landing beside the injected thinking object
    /// in a single request. Where the sibling test above proves flattened
    /// extra_body coexistence, this one exercises the flags themselves
    /// (flag-gated injection plus the forced temperature and budget-safe
    /// output limit that come with it).
    #[tokio::test]
    async fn cache_and_thinking_passthrough_flags_compose_in_one_request() {
        let (provider, captured, server) = mock_streaming_cache_capture(true).await;
        let provider = OpenAiCompatibleModelProvider {
            thinking_passthrough: true,
            ..provider
        };
        let messages = vec![
            ChatMessage::system("you are brief"),
            ChatMessage::user("hi"),
            ChatMessage::assistant("hello"),
            ChatMessage::user("bye"),
        ];
        let result = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: Some(zeroclaw_api::model_provider::NativeThinkingParams {
                        budget_tokens: 2048,
                        display: None,
                    }),
                },
                "test-model",
                Some(0.7),
            )
            .await;
        server.abort();
        result.unwrap_or_else(|error| panic!("two-flag request failed: {error}"));
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]["thinking"],
            serde_json::json!({"type": "enabled", "budget_tokens": 2048}),
            "the flag-injected thinking object rides at the top level"
        );
        assert_eq!(
            requests[0]["temperature"],
            serde_json::json!(1.0),
            "thinking injection forces temperature 1.0 on the combined request"
        );
        assert_eq!(
            requests[0]["messages"][0]["content"][0]["cache_control"],
            serde_json::json!({"type": "ephemeral"}),
            "system cache breakpoint present alongside flag-injected thinking"
        );
        assert_eq!(
            requests[0]["messages"][3]["content"][0]["cache_control"],
            serde_json::json!({"type": "ephemeral"}),
            "rolling cache breakpoint present alongside flag-injected thinking"
        );
        let max_tokens = requests[0]["max_tokens"]
            .as_u64()
            .expect("budget-safe output limit is set");
        assert!(
            max_tokens > 2048,
            "output limit must strictly exceed the thinking budget, got {max_tokens}"
        );
    }

    #[test]
    fn api_response_parses_without_usage() {
        let json = r#"{"choices": [{"message": {"content": "Hello"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.usage.is_none());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // merged-system carrier + usage-capture boundary tests
    // ═══════════════════════════════════════════════════════════════════════

    /// Merged-carrier, single-turn: with `merge_system_into_user`, the
    /// system role never reaches the wire, so the merged first user message
    /// is the only system-equivalent carrier and must carry the breakpoint
    /// unconditionally. Before the repair this shape sent zero breakpoints.
    #[tokio::test]
    async fn cache_passthrough_merged_system_marks_first_user_single_turn() {
        let (provider, captured, server) = mock_streaming_cache_capture(true).await;
        let provider = OpenAiCompatibleModelProvider {
            merge_system_into_user: true,
            ..provider
        };
        let messages = vec![
            ChatMessage::system("core policy"),
            ChatMessage::user("hello"),
        ];
        let result = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test-model",
                None,
            )
            .await;
        server.abort();
        result.unwrap_or_else(|error| panic!("merged single-turn request failed: {error}"));
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]["messages"],
            serde_json::json!([
                {"role": "user", "content": [
                    {"type": "text", "text": "core policy\n\nhello",
                     "cache_control": {"type": "ephemeral"}},
                ]},
            ]),
            "merged user message is the system-equivalent carrier and must be marked"
        );
        assert_eq!(
            requests[0].to_string().matches("cache_control").count(),
            1,
            "single exchange carries exactly the carrier breakpoint"
        );
    }

    /// Merged-carrier, multi-turn: the carrier keeps the system breakpoint
    /// and the rolling breakpoint still lands on the last message, exactly
    /// two total, middle messages untouched.
    #[tokio::test]
    async fn cache_passthrough_merged_system_carrier_plus_rolling_multi_turn() {
        let (provider, captured, server) = mock_streaming_cache_capture(true).await;
        let provider = OpenAiCompatibleModelProvider {
            merge_system_into_user: true,
            ..provider
        };
        let messages = vec![
            ChatMessage::system("core policy"),
            ChatMessage::user("hi"),
            ChatMessage::assistant("hello"),
            ChatMessage::user("bye"),
        ];
        let result = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test-model",
                None,
            )
            .await;
        server.abort();
        result.unwrap_or_else(|error| panic!("merged multi-turn request failed: {error}"));
        let requests = captured.lock().unwrap();
        assert_eq!(
            requests[0]["messages"],
            serde_json::json!([
                {"role": "user", "content": [
                    {"type": "text", "text": "core policy\n\nhi",
                     "cache_control": {"type": "ephemeral"}},
                ]},
                {"role": "assistant", "content": "hello"},
                {"role": "user", "content": [
                    {"type": "text", "text": "bye",
                     "cache_control": {"type": "ephemeral"}},
                ]},
            ]),
            "merged carrier and rolling breakpoint with middle messages untouched"
        );
        assert_eq!(
            requests[0].to_string().matches("cache_control").count(),
            2,
            "merged multi-turn must carry exactly two breakpoints"
        );
    }

    /// Merged-carrier, assistant-first history: the merged user is not
    /// message zero; the carrier index must follow the first user wherever
    /// it sits.
    #[tokio::test]
    async fn cache_passthrough_merged_system_marks_first_user_after_assistant() {
        let (provider, captured, server) = mock_streaming_cache_capture(true).await;
        let provider = OpenAiCompatibleModelProvider {
            merge_system_into_user: true,
            ..provider
        };
        let messages = vec![
            ChatMessage::assistant("ack"),
            ChatMessage::system("core policy"),
            ChatMessage::user("hello"),
        ];
        let result = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test-model",
                None,
            )
            .await;
        server.abort();
        result.unwrap_or_else(|error| panic!("assistant-first merged request failed: {error}"));
        let requests = captured.lock().unwrap();
        assert_eq!(
            requests[0]["messages"],
            serde_json::json!([
                {"role": "assistant", "content": "ack"},
                {"role": "user", "content": [
                    {"type": "text", "text": "core policy\n\nhello",
                     "cache_control": {"type": "ephemeral"}},
                ]},
            ]),
            "carrier breakpoint lands on the merged user, not on message zero"
        );
        assert_eq!(
            requests[0].to_string().matches("cache_control").count(),
            1,
            "assistant + merged user is a single exchange: no rolling breakpoint"
        );
    }

    /// Streaming twin of the merged-carrier pin.
    #[tokio::test]
    async fn cache_passthrough_merged_system_streaming_matches_non_streaming() {
        use futures_util::StreamExt as _;

        let (provider, captured, server) = mock_streaming_cache_capture(true).await;
        let provider = OpenAiCompatibleModelProvider {
            merge_system_into_user: true,
            ..provider
        };
        let events = provider
            .stream_chat(
                ProviderChatRequest {
                    messages: &[ChatMessage::system("be brief"), ChatMessage::user("hello")],
                    tools: None,
                    thinking: None,
                },
                "test-model",
                None,
                StreamOptions {
                    enabled: true,
                    count_tokens: false,
                },
            )
            .collect::<Vec<_>>()
            .await;
        server.abort();
        assert!(
            events.iter().all(Result::is_ok),
            "merged streaming request must succeed: {events:?}"
        );
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]["messages"],
            serde_json::json!([
                {"role": "user", "content": [
                    {"type": "text", "text": "be brief\n\nhello",
                     "cache_control": {"type": "ephemeral"}},
                ]},
            ]),
            "streaming merged carrier carries the breakpoint identically"
        );
    }

    /// Flag-off merged: the merge behavior is unchanged and the wire stays
    /// free of markers.
    #[tokio::test]
    async fn cache_passthrough_merged_flag_off_body_unmarked() {
        let (provider, captured, server) = mock_streaming_cache_capture(false).await;
        let provider = OpenAiCompatibleModelProvider {
            merge_system_into_user: true,
            ..provider
        };
        let messages = vec![
            ChatMessage::system("core policy"),
            ChatMessage::user("hello"),
        ];
        let result = provider
            .chat(
                ProviderChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test-model",
                None,
            )
            .await;
        server.abort();
        result.unwrap_or_else(|error| panic!("flag-off merged request failed: {error}"));
        let requests = captured.lock().unwrap();
        assert_eq!(
            requests[0]["messages"],
            serde_json::json!([
                {"role": "user", "content": "core policy\n\nhello"},
            ]),
            "flag-off merged body must be byte-identical to the pre-feature wire"
        );
        assert!(
            !requests[0].to_string().contains("cache_control"),
            "flag-off merged request must carry no cache markers"
        );
    }

    /// Capture-boundary regression: the text-only helpers must never emit
    /// cache breakpoints even with the flag on, because their responses
    /// drop usage and could never account for the premium.
    #[tokio::test]
    async fn cache_passthrough_simple_paths_emit_no_breakpoints_even_when_enabled() {
        let (provider, captured, server) = mock_streaming_cache_capture(true).await;
        let _ = provider
            .chat_with_system(Some("be brief"), "hello", "test-model", None)
            .await;
        let messages = vec![
            ChatMessage::system("you are brief"),
            ChatMessage::user("hi"),
            ChatMessage::assistant("hello"),
            ChatMessage::user("bye"),
        ];
        let _ = provider
            .chat_with_history(&messages, "test-model", None)
            .await;
        server.abort();
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 2);
        for (index, request) in requests.iter().enumerate() {
            assert!(
                !request.to_string().contains("cache_control"),
                "simple path {index} must not emit cache breakpoints: {request}"
            );
        }
    }

    /// Capture-boundary regression, streaming side: the legacy chunk stream
    /// drops usage, so it must not trigger premium writes either.
    #[tokio::test]
    async fn cache_passthrough_legacy_stream_emits_no_breakpoints_even_when_enabled() {
        use futures_util::StreamExt as _;

        let (provider, captured, server) = mock_streaming_cache_capture(true).await;
        let events = provider
            .stream_chat_with_system(
                Some("be brief"),
                "hello",
                "test-model",
                None,
                StreamOptions {
                    enabled: true,
                    count_tokens: false,
                },
            )
            .collect::<Vec<_>>()
            .await;
        server.abort();
        let _ = events;
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            !requests[0].to_string().contains("cache_control"),
            "legacy chunk stream must carry no cache markers even with the flag on"
        );
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
            thinking_blocks: None,
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
            thinking_blocks: None,
        };

        let parsed = provider.parse_native_response(message);
        assert!(parsed.reasoning_content.is_none());
        assert_eq!(parsed.text.as_deref(), Some("hello"));
    }

    #[test]
    fn thinking_passthrough_off_keeps_reasoning_content_raw() {
        // Flag off = today's behavior: `reasoning_content` passes through
        // untouched and gateway `thinking_blocks` are ignored entirely.
        let provider = make_model_provider("test", "https://example.com", None);
        let message = ResponseMessage {
            content: None,
            reasoning_content: Some("raw chain of thought".to_string()),
            tool_calls: None,
            thinking_blocks: Some(vec![serde_json::json!({
                "type": "thinking",
                "thinking": "signed thought",
                "signature": "sig"
            })]),
        };

        let parsed = provider.parse_native_response(message);
        assert_eq!(
            parsed.reasoning_content.as_deref(),
            Some("raw chain of thought"),
            "flag-off capture must not normalize or wrap the reasoning string"
        );
    }

    #[test]
    fn thinking_passthrough_capture_normalizes_thinking_blocks_to_signed_lines() {
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let message = ResponseMessage {
            content: None,
            reasoning_content: None,
            tool_calls: None,
            thinking_blocks: Some(vec![
                serde_json::json!({"type": "thinking", "thinking": "first", "signature": "sig1"}),
                serde_json::json!({"type": "thinking", "thinking": "second", "signature": "sig2"}),
            ]),
        };

        let parsed = provider.parse_native_response(message);
        let reasoning = parsed
            .reasoning_content
            .expect("blocks must normalize to lines");
        let lines: Vec<serde_json::Value> = reasoning
            .split('\n')
            .map(|line| serde_json::from_str(line).expect("each line must be valid JSON"))
            .collect();
        assert_eq!(
            lines,
            vec![
                serde_json::json!({"thinking": "first", "signature": "sig1"}),
                serde_json::json!({"thinking": "second", "signature": "sig2"}),
            ],
            "capture must emit one signed-JSON line per thinking block, matching the \
             native provider's reasoning_content format"
        );
    }

    #[test]
    fn thinking_passthrough_capture_wraps_bare_reasoning_string_as_unsigned_line() {
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let message = ResponseMessage {
            content: None,
            reasoning_content: Some("plain reasoning".to_string()),
            tool_calls: None,
            thinking_blocks: None,
        };

        let parsed = provider.parse_native_response(message);
        let reasoning = parsed.reasoning_content.expect("string must be captured");
        let line: serde_json::Value =
            serde_json::from_str(&reasoning).expect("captured string must be a JSON line");
        assert_eq!(
            line,
            serde_json::json!({"thinking": "plain reasoning", "signature": ""}),
            "bare reasoning strings normalize to an unsigned envelope line"
        );
    }

    #[test]
    fn thinking_passthrough_capture_signed_blocks_win_over_plain_string() {
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let message = ResponseMessage {
            content: None,
            reasoning_content: Some("unsigned duplicate".to_string()),
            tool_calls: None,
            thinking_blocks: Some(vec![serde_json::json!({
                "thinking": "signed", "signature": "sig"
            })]),
        };

        let parsed = provider.parse_native_response(message);
        let reasoning = parsed
            .reasoning_content
            .expect("capture must produce lines");
        let line: serde_json::Value = serde_json::from_str(&reasoning).unwrap();
        assert_eq!(
            line,
            serde_json::json!({"thinking": "signed", "signature": "sig"}),
            "signed thinking_blocks are the authoritative capture source"
        );
    }

    #[test]
    fn thinking_passthrough_capture_skips_empty_blocks_and_falls_back_to_string() {
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let message = ResponseMessage {
            content: None,
            reasoning_content: Some("fallback thought".to_string()),
            tool_calls: None,
            thinking_blocks: Some(vec![
                serde_json::json!({"type": "thinking", "thinking": "", "signature": ""}),
                serde_json::json!({"thinking": "", "signature": ""}),
            ]),
        };

        let parsed = provider.parse_native_response(message);
        let reasoning = parsed.reasoning_content.expect("fallback capture");
        let line: serde_json::Value = serde_json::from_str(&reasoning).unwrap();
        assert_eq!(
            line,
            serde_json::json!({"thinking": "fallback thought", "signature": ""}),
            "blocks with neither text nor signature are skipped; bare string is the fallback"
        );
    }

    #[test]
    fn thinking_passthrough_replay_reconstructs_signed_blocks() {
        // Flag on + signed envelope history (2a capture format) -> outbound
        // thinking_blocks; string fields suppressed.
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let signed = r#"{"content": "answer", "reasoning_content": "{\"thinking\":\"thought one\",\"signature\":\"sig1\"}\n{\"thinking\":\"thought two\",\"signature\":\"sig2\"}", "tool_calls": []}"#;
        let messages = vec![ChatMessage::assistant(signed.to_string())];

        let native = provider.convert_messages_for_native(&messages, false);
        assert_eq!(native.len(), 1);
        let message = &native[0];
        assert_eq!(
            message.reasoning_content, None,
            "signed replay must suppress the reasoning_content string"
        );
        assert_eq!(message.reasoning, None);
        let blocks = message
            .thinking_blocks
            .as_ref()
            .expect("signed history must reconstruct thinking_blocks");
        assert_eq!(
            blocks,
            &vec![
                serde_json::json!({"type": "thinking", "thinking": "thought one", "signature": "sig1"}),
                serde_json::json!({"type": "thinking", "thinking": "thought two", "signature": "sig2"}),
            ],
            "reconstructed blocks must be Anthropic content-block shaped"
        );
    }

    #[test]
    fn thinking_passthrough_replay_signature_only_history_round_trips_probe_shape() {
        // Live-probe fixture (slice 2b): 2a captures a signature-only block;
        // replay must reconstruct exactly what the gateway sent.
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let signature_only = r#"{"content": "answer", "reasoning_content": "{\"thinking\":\"\",\"signature\":\"CAISkQIKjwEIERgCKkD/bXd1ajb4AEMrh8seIyvE22xRnQ==\"}", "tool_calls": []}"#;
        let messages = vec![ChatMessage::assistant(signature_only.to_string())];

        let native = provider.convert_messages_for_native(&messages, false);
        let blocks = native[0]
            .thinking_blocks
            .as_ref()
            .expect("signature-only history must reconstruct thinking_blocks");
        assert_eq!(
            blocks,
            &vec![serde_json::json!({
                "type": "thinking",
                "thinking": "",
                "signature": "CAISkQIKjwEIERgCKkD/bXd1ajb4AEMrh8seIyvE22xRnQ=="
            })],
            "signature-only capture must round-trip to the observed gateway block shape"
        );
    }

    #[test]
    fn thinking_passthrough_replay_unsigned_history_is_noop() {
        // Unsigned envelope (signature "") and plain unparseable text both
        // send nothing: blocks are never fabricated, and a bare string is
        // exactly what translating gateways reject.
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let messages = vec![
            ChatMessage::assistant(
                r#"{"content": "a", "reasoning_content": "{\"thinking\":\"t\",\"signature\":\"\"}"}"#
                    .to_string(),
            ),
            ChatMessage::assistant(
                r#"{"content": "b", "reasoning_content": "plain streamed reasoning"}"#.to_string(),
            ),
        ];

        let native = provider.convert_messages_for_native(&messages, false);
        assert_eq!(native.len(), 2);
        for message in &native {
            assert_eq!(
                message.reasoning_content, None,
                "unsigned history replays nothing"
            );
            assert_eq!(message.reasoning, None);
            assert!(message.thinking_blocks.is_none(), "no fabricated blocks");
        }
    }

    #[test]
    fn thinking_passthrough_replay_mixed_lines_noop_entirely() {
        // All-or-nothing: one unsigned line in signed history voids the
        // replay rather than sending a partial block sequence.
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let mixed = r#"{"content": "answer", "reasoning_content": "{\"thinking\":\"signed\",\"signature\":\"sig\"}\n{\"thinking\":\"unsigned\",\"signature\":\"\"}", "tool_calls": []}"#;
        let messages = vec![ChatMessage::assistant(mixed.to_string())];

        let native = provider.convert_messages_for_native(&messages, false);
        assert!(native[0].thinking_blocks.is_none());
        assert_eq!(native[0].reasoning_content, None);
    }

    #[test]
    fn thinking_passthrough_replay_flag_off_replays_strings_verbatim() {
        // Flag off = today's behavior: strings replay, no blocks field.
        let provider = make_model_provider("gateway", "https://example.com", None);
        let signed = r#"{"content": "answer", "reasoning_content": "{\"thinking\":\"t\",\"signature\":\"sig\"}", "tool_calls": []}"#;
        let messages = vec![ChatMessage::assistant(signed.to_string())];

        let native = provider.convert_messages_for_native(&messages, false);
        assert_eq!(
            native[0].reasoning_content.as_deref(),
            Some(r#"{"thinking":"t","signature":"sig"}"#),
            "flag-off replay must pass the stored string through untouched"
        );
        assert!(native[0].thinking_blocks.is_none());
    }

    #[test]
    fn thinking_passthrough_replay_replay_override_wins_over_flag() {
        // replay_assistant_reasoning = false wins under thinking_passthrough:
        // providers that reject reasoning input still get nothing.
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .without_assistant_reasoning_replay()
            .build();
        let signed = r#"{"content": "answer", "reasoning_content": "{\"thinking\":\"t\",\"signature\":\"sig\"}", "tool_calls": []}"#;
        let messages = vec![ChatMessage::assistant(signed.to_string())];

        let native = provider.convert_messages_for_native(&messages, false);
        assert_eq!(native[0].reasoning_content, None);
        assert_eq!(native[0].reasoning, None);
        assert!(native[0].thinking_blocks.is_none());
    }

    #[test]
    fn thinking_passthrough_replay_blocks_serialize_top_level() {
        // The reconstructed blocks serialize as a top-level assistant
        // message field for the gateway to translate.
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let signed = r#"{"content": "answer", "reasoning_content": "{\"thinking\":\"t\",\"signature\":\"sig\"}", "tool_calls": []}"#;
        let messages = vec![ChatMessage::assistant(signed.to_string())];

        let native = provider.convert_messages_for_native(&messages, false);
        let value = serde_json::to_value(&native[0]).unwrap();
        assert_eq!(
            value["thinking_blocks"],
            serde_json::json!([{"type": "thinking", "thinking": "t", "signature": "sig"}]),
            "thinking_blocks must serialize at the assistant message top level"
        );
    }

    #[test]
    fn thinking_passthrough_disables_streaming_capability() {
        // Passthrough pins requests to the non-streaming wire: gateway SSE
        // thinking frames are unverified, and streamed reasoning would reach
        // history unsigned and break signed replay. Flag off keeps streaming.
        let with_flag = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let without_flag = make_model_provider("gateway", "https://example.com", None);

        assert!(
            !with_flag.supports_streaming(),
            "passthrough must report the provider as non-streaming"
        );
        assert!(without_flag.supports_streaming());
    }

    #[test]
    fn thinking_passthrough_capture_preserves_redacted_blocks_verbatim() {
        // redacted_thinking carries an opaque data field and no signature;
        // the empty-empty skip must never swallow it. Non-thinking blocks
        // are stored as raw JSON lines.
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let redacted = serde_json::json!({
            "type": "redacted_thinking",
            "data": "ErUBCkEIRAP...opaque"
        });
        let message = ResponseMessage {
            content: Some("answer".to_string()),
            reasoning_content: None,
            tool_calls: None,
            thinking_blocks: Some(vec![
                serde_json::json!({
                    "type": "thinking",
                    "thinking": "visible thought",
                    "signature": "sig1"
                }),
                redacted.clone(),
            ]),
        };

        let parsed = provider.parse_native_response(message);
        let reasoning = parsed
            .reasoning_content
            .expect("both blocks must be captured");
        let lines: Vec<serde_json::Value> = reasoning
            .split('\n')
            .map(serde_json::from_str)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(lines.len(), 2, "thinking and redacted lines both captured");
        assert_eq!(
            lines[0],
            serde_json::json!({"thinking": "visible thought", "signature": "sig1"}),
        );
        assert_eq!(
            lines[1], redacted,
            "redacted_thinking must be stored verbatim, data field intact"
        );
    }

    #[test]
    fn thinking_passthrough_redacted_blocks_round_trip_verbatim() {
        // Full circle: gateway blocks -> capture lines -> replay blocks, with
        // the redacted block byte-identical and the thinking block canonical.
        let redacted = serde_json::json!({
            "type": "redacted_thinking",
            "data": "ErUBCkEIRAP...opaque"
        });
        let thinking = serde_json::json!({
            "type": "thinking",
            "thinking": "visible thought",
            "signature": "sig1"
        });
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();

        let message = ResponseMessage {
            content: Some("answer".to_string()),
            reasoning_content: None,
            tool_calls: None,
            thinking_blocks: Some(vec![thinking.clone(), redacted.clone()]),
        };
        let reasoning = provider
            .parse_native_response(message)
            .reasoning_content
            .expect("capture must produce replay lines");

        let blocks = OpenAiCompatibleModelProvider::reasoning_lines_to_thinking_blocks(&reasoning)
            .expect("signed thinking plus redacted lines must reconstruct");
        assert_eq!(
            blocks,
            vec![thinking, redacted],
            "replay must emit the thinking block canonical and the redacted block verbatim"
        );
    }

    #[test]
    fn thinking_passthrough_capture_skips_hostile_block_types() {
        // Whitelist enforcement: only thinking and redacted_thinking are
        // captured. A gateway-controlled text/tool_use/unknown-typed block
        // must not become stored replay material.
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let message = ResponseMessage {
            content: Some("answer".to_string()),
            reasoning_content: None,
            tool_calls: None,
            thinking_blocks: Some(vec![
                serde_json::json!({"type": "text", "text": "hostile text block"}),
                serde_json::json!({
                    "type": "tool_use",
                    "id": "call_x",
                    "name": "get_weather",
                    "input": {"city": "SF"}
                }),
                serde_json::json!({"type": "totally_novel", "payload": "???"}),
                serde_json::json!({
                    "type": "thinking",
                    "thinking": "real thought",
                    "signature": "sig1"
                }),
            ]),
        };

        let parsed = provider.parse_native_response(message);
        let reasoning = parsed
            .reasoning_content
            .expect("the thinking block must be captured");
        let lines: Vec<&str> = reasoning.split('\n').collect();
        assert_eq!(
            lines.len(),
            1,
            "hostile block types must not reach replay history"
        );
        assert!(lines[0].contains("real thought"));
        assert!(!lines[0].contains("hostile"));
        assert!(!lines[0].contains("tool_use"));
        assert!(!lines[0].contains("totally_novel"));
    }

    #[test]
    fn thinking_passthrough_capture_rejects_malformed_redacted_blocks() {
        // redacted_thinking without a non-empty data field fails validation
        // and is skipped, not stored verbatim.
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let message = ResponseMessage {
            content: Some("answer".to_string()),
            reasoning_content: None,
            tool_calls: None,
            thinking_blocks: Some(vec![
                serde_json::json!({"type": "redacted_thinking"}),
                serde_json::json!({"type": "redacted_thinking", "data": ""}),
                serde_json::json!({"type": "redacted_thinking", "data": "ErUBCkEIRAP"}),
            ]),
        };

        let parsed = provider.parse_native_response(message);
        let reasoning = parsed
            .reasoning_content
            .expect("the well-formed block must be captured");
        let lines: Vec<&str> = reasoning.split('\n').collect();
        assert_eq!(lines.len(), 1, "only the validated redacted block survives");
        assert!(lines[0].contains("ErUBCkEIRAP"));
    }

    #[test]
    fn thinking_passthrough_replay_does_not_forward_hostile_lines() {
        // History written before the whitelist (or tampered) must not forward
        // unknown-typed blocks into outbound thinking_blocks. Well-formed
        // siblings still replay.
        let reasoning = format!(
            "{}\n{}\n{}",
            r#"{"type":"text","text":"hostile"}"#,
            r#"{"type":"tool_use","id":"call_x","name":"f","input":{}}"#,
            r#"{"type":"thinking","thinking":"real","signature":"sig1"}"#
        );

        let blocks = OpenAiCompatibleModelProvider::reasoning_lines_to_thinking_blocks(&reasoning)
            .expect("a valid signed line exists");
        assert_eq!(
            blocks,
            vec![serde_json::json!({
                "type": "thinking",
                "thinking": "real",
                "signature": "sig1"
            })],
            "hostile typed lines must be skipped, valid thinking forwarded"
        );
    }

    #[test]
    fn thinking_passthrough_replay_skips_malformed_redacted_lines() {
        // A redacted line without data cannot come from fixed capture; it
        // must not forward. Signed thinking siblings still replay.
        let reasoning = format!(
            "{}\n{}",
            r#"{"type":"redacted_thinking"}"#,
            r#"{"type":"thinking","thinking":"t","signature":"sig"}"#
        );

        let blocks = OpenAiCompatibleModelProvider::reasoning_lines_to_thinking_blocks(&reasoning)
            .expect("a valid signed line exists");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], serde_json::json!("thinking"));
    }

    #[tokio::test]
    async fn thinking_passthrough_tool_loop_replays_signed_blocks_on_turn_two() {
        // Two-turn regression: a tool-call turn whose response carried signed
        // thinking must produce a turn-2 outbound request replaying those
        // exact blocks (non-streaming; the flag pins the loop to this path).
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let envelope = r#"{"thinking":"checked the weather","signature":"sig1"}"#;
        let assistant_content = serde_json::json!({
            "content": null,
            "reasoning_content": envelope,
            "tool_calls": [
                {
                    "id": "call_1",
                    "name": "get_weather",
                    "arguments": "{\"city\": \"SF\"}",
                }
            ],
        });
        let history = vec![
            ChatMessage::user("What is the weather in SF?"),
            ChatMessage::assistant(assistant_content.to_string()),
            ChatMessage::tool(
                r#"{"tool_call_id": "call_1", "content": "72F and sunny"}"#.to_string(),
            ),
        ];

        let native = provider.convert_messages_for_native(&history, false);
        assert_eq!(native.len(), 3, "all three turns convert");
        let assistant = &native[1];
        assert_eq!(
            assistant.reasoning_content, None,
            "signed replay suppresses the string fields"
        );
        assert_eq!(
            assistant.thinking_blocks,
            Some(vec![serde_json::json!({
                "type": "thinking",
                "thinking": "checked the weather",
                "signature": "sig1"
            })]),
            "turn-2 outbound must replay the exact signed blocks from turn 1"
        );
        assert!(assistant.tool_calls.is_some(), "tool calls stay intact");
        let tool_result = &native[2];
        assert_eq!(tool_result.role, "tool");
        assert!(tool_result.thinking_blocks.is_none());
    }

    #[test]
    fn thinking_passthrough_capture_signature_only_block_survives() {
        // Live-probe fixture (slice 2b, .worktree/probe-truefoundry-2026-09-03.md):
        // TrueFoundry returns thinking blocks with EMPTY text but a valid
        // signature. These must survive capture — a text-only gate would drop
        // them and break replay (the same bug previously fixed for the native
        // provider's signature-only blocks).
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let message = ResponseMessage {
            content: Some("answer".to_string()),
            reasoning_content: None,
            tool_calls: None,
            thinking_blocks: Some(vec![serde_json::json!({
                "type": "thinking",
                "thinking": "",
                "signature": "CAISkQIKjwEIERgCKkD/bXd1ajb4AEMrh8seIyvE22xRnQ=="
            })]),
        };

        let parsed = provider.parse_native_response(message);
        let reasoning = parsed
            .reasoning_content
            .expect("signature-only block must be captured");
        let line: serde_json::Value = serde_json::from_str(&reasoning).unwrap();
        assert_eq!(
            line,
            serde_json::json!({
                "thinking": "",
                "signature": "CAISkQIKjwEIERgCKkD/bXd1ajb4AEMrh8seIyvE22xRnQ=="
            }),
            "signature-only blocks (empty thinking text) must survive capture intact"
        );
    }

    #[test]
    fn thinking_passthrough_capture_parses_gateway_thinking_blocks_json() {
        // End-to-end through the response envelope: a gateway response whose
        // message carries `thinking_blocks` deserializes into the raw field
        // that capture normalization reads.
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("gateway")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .with_thinking_passthrough()
            .build();
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "answer",
                    "reasoning_content": "plain text",
                    "thinking_blocks": [
                        {"type": "thinking", "thinking": "block thought", "signature": "sig9"}
                    ]
                }
            }]
        });
        let response =
            parse_chat_response_body("gateway", &body.to_string()).expect("payload parses");
        let parsed = provider.parse_native_response(
            response
                .choices
                .into_iter()
                .next()
                .expect("one choice")
                .message,
        );
        let reasoning = parsed.reasoning_content.expect("capture from gateway JSON");
        let line: serde_json::Value = serde_json::from_str(&reasoning).unwrap();
        assert_eq!(
            line,
            serde_json::json!({"thinking": "block thought", "signature": "sig9"}),
            "gateway thinking_blocks must reach capture and win over the plain string"
        );
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
    fn convert_messages_for_native_round_trips_tool_call_extra_content() {
        let extra_content = serde_json::json!({
            "google": {
                "thought_signature": "sig_1"
            }
        });
        let history_json = serde_json::json!({
            "content": "",
            "tool_calls": [{
                "id": "tc_1",
                "name": "shell",
                "arguments": "{\"cmd\":\"ls\"}",
                "extra_content": extra_content.clone()
            }]
        });

        let messages = vec![ChatMessage::assistant(history_json.to_string())];
        let provider = make_model_provider("test", "https://example.com", None);
        let native = provider.convert_messages_for_native(&messages, true);
        let tool_calls = native[0].tool_calls.as_ref().unwrap();

        assert_eq!(tool_calls[0].extra_content.as_ref(), Some(&extra_content));
    }

    #[test]
    fn groq_outbound_omits_reasoning_replay_but_default_preserves_it() {
        let history_json = serde_json::json!({
            "content": "I will check",
            "tool_calls": [{
                "id": "tc_1",
                "name": "shell",
                "arguments": "{\"cmd\":\"ls\"}"
            }],
            "reasoning_content": "canonical thought",
            "reasoning": "alias thought"
        });

        let messages = vec![ChatMessage::assistant(history_json.to_string())];
        let default_provider =
            make_model_provider("OpenRouter", "https://openrouter.ai/api/v1", None);
        let default_request = default_provider.build_native_tool_chat_request(
            &messages,
            None,
            "openai/gpt-oss-120b",
            None,
            true,
            false,
            None,
        );
        let default_message = &default_request.messages[0];
        assert_eq!(default_message.role, "assistant");
        assert_eq!(
            default_message.reasoning_content.as_deref(),
            Some("canonical thought")
        );
        // Default provider preserves BOTH field names faithfully so the value
        // round-trips on multi-turn requests `reasoning_content` and
        // `reasoning` are carried independently, not collapsed into one.
        assert_eq!(default_message.reasoning.as_deref(), Some("alias thought"));
        assert!(default_message.tool_calls.is_some());
        let default_json = serde_json::to_value(default_message).unwrap();
        assert_eq!(
            default_json.get("reasoning_content"),
            Some(&serde_json::json!("canonical thought"))
        );
        assert_eq!(
            default_json.get("reasoning"),
            Some(&serde_json::json!("alias thought"))
        );

        let groq_provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("Groq")
            .base_url("https://api.groq.com/openai/v1")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .without_assistant_reasoning_replay()
            .build();
        let groq_request = groq_provider.build_native_tool_chat_request(
            &messages,
            None,
            "openai/gpt-oss-120b",
            None,
            true,
            false,
            None,
        );
        let groq_message = &groq_request.messages[0];
        assert_eq!(groq_message.role, "assistant");
        assert!(groq_message.reasoning_content.is_none());
        assert!(groq_message.tool_calls.is_some());
        let groq_json = serde_json::to_value(groq_message).unwrap();
        assert_eq!(groq_json.get("role"), Some(&serde_json::json!("assistant")));
        assert_eq!(
            groq_json
                .get("tool_calls")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(1)
        );
        assert!(groq_json.get("reasoning_content").is_none());
        assert!(groq_json.get("reasoning").is_none());
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
    fn convert_messages_for_native_omits_empty_tool_call_content() {
        let empty_history_json = serde_json::json!({
            "content": "",
            "tool_calls": [{
                "id": "tc_1",
                "name": "shell",
                "arguments": "{\"cmd\":\"ls\"}"
            }]
        });
        let non_empty_history_json = serde_json::json!({
            "content": "I will check",
            "tool_calls": [{
                "id": "tc_2",
                "name": "shell",
                "arguments": "{\"cmd\":\"pwd\"}"
            }]
        });

        let messages = vec![
            ChatMessage::assistant(empty_history_json.to_string()),
            ChatMessage::assistant(non_empty_history_json.to_string()),
        ];
        let provider = make_model_provider("test", "https://example.com", None);
        let native = provider.convert_messages_for_native(&messages, true);
        let empty_json = serde_json::to_value(&native[0]).unwrap();
        let non_empty_json = serde_json::to_value(&native[1]).unwrap();

        assert_eq!(empty_json.get("content"), None);
        assert_ne!(
            empty_json.get("content"),
            Some(&serde_json::Value::String(String::new()))
        );
        assert_eq!(
            non_empty_json.get("content"),
            Some(&serde_json::json!("I will check"))
        );
    }

    #[test]
    fn convert_messages_for_native_sends_string_tool_call_content_on_cloudflare() {
        // Cloudflare Workers AI rejects an assistant tool-call message whose
        // `content` is absent or null (HTTP 400, AiError 5006). Measured:
        // content=null -> 400, content omitted -> 400, content="" -> 200.
        // Every other backend keeps the omitting behaviour pinned by
        // convert_messages_for_native_omits_empty_tool_call_content.
        let history_json = serde_json::json!({
            "content": "",
            "tool_calls": [{
                "id": "tc_1",
                "name": "realms_proposal_firewall",
                "arguments": "{}"
            }]
        });
        let messages = vec![ChatMessage::assistant(history_json.to_string())];

        let cloudflare = make_model_provider(
            "workers_ai",
            "https://api.cloudflare.com/client/v4/accounts/acct/ai/v1/chat/completions",
            None,
        );
        let native = cloudflare.convert_messages_for_native(&messages, true);
        let json = serde_json::to_value(&native[0]).unwrap();
        assert_eq!(
            json.get("content"),
            Some(&serde_json::Value::String(String::new())),
            "Cloudflare must receive content as a string, not an omitted field"
        );

        let other = make_model_provider("test", "https://example.com", None);
        let native = other.convert_messages_for_native(&messages, true);
        let json = serde_json::to_value(&native[0]).unwrap();
        assert_eq!(
            json.get("content"),
            None,
            "non-Cloudflare backends keep the existing omitting behaviour"
        );
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
            reasoning: None,
            thinking_blocks: None,
            name: None,
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
            reasoning: None,
            thinking_blocks: None,
            name: None,
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
    fn stream_idle_timeout_keeps_300s_floor_when_timeout_secs_is_lower() {
        assert_eq!(
            crate::stream_idle_timeout(120),
            crate::StreamIdleBound::Configurable(std::time::Duration::from_secs(300))
        );
        assert_eq!(
            crate::stream_idle_timeout(300),
            crate::StreamIdleBound::Configurable(std::time::Duration::from_secs(300))
        );
    }

    #[test]
    fn stream_idle_timeout_raises_bound_when_timeout_secs_exceeds_floor() {
        assert_eq!(
            crate::stream_idle_timeout(301),
            crate::StreamIdleBound::Configurable(std::time::Duration::from_secs(301))
        );
        assert_eq!(
            crate::stream_idle_timeout(3600),
            crate::StreamIdleBound::Configurable(std::time::Duration::from_secs(3600))
        );
    }

    #[tokio::test]
    async fn stream_idle_error_message_names_bound_on_read_timeout() {
        // Accept the connection, then never write a byte: the read-idle bound
        // fires while the streaming client waits for response headers.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = ::zeroclaw_spawn::spawn!(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            futures_util::future::pending::<()>().await;
        });
        let client = reqwest::Client::builder()
            .read_timeout(std::time::Duration::from_millis(100))
            .build()
            .unwrap();
        let err = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.get(format!("http://{addr}/stream")).send(),
        )
        .await
        .expect("stalled headers must hit the read-idle bound")
        .unwrap_err();
        server.abort();
        assert!(
            err.is_timeout(),
            "stalled headers must surface as a timeout: {err}"
        );
        assert!(!err.is_connect(), "the connection itself succeeded: {err}");
        let message = crate::stream_idle_error_message(
            &err,
            crate::StreamIdleBound::Configurable(std::time::Duration::from_secs(300)),
        );
        assert!(
            message.contains("no data from provider for 300s"),
            "idle message must name the bound that fired: {message}"
        );
        assert!(
            message.contains("stream idle timeout"),
            "idle message must name the idle timeout: {message}"
        );
        assert!(
            message.contains("raise timeout_secs above 300s"),
            "idle message must name the knob that raises the bound: {message}"
        );
        assert!(
            message.contains(&crate::format_error_chain(&err)),
            "the underlying reqwest error must stay in the chain: {message}"
        );
    }

    #[tokio::test]
    async fn stream_idle_error_message_leaves_connect_errors_unchanged() {
        let err = reqwest::Client::new()
            .get("http://127.0.0.1:1/stream")
            .send()
            .await
            .unwrap_err();
        assert!(
            err.is_connect(),
            "a refused local port is a connect error: {err}"
        );
        let message = crate::stream_idle_error_message(
            &err,
            crate::StreamIdleBound::Configurable(std::time::Duration::from_secs(300)),
        );
        assert_eq!(
            message,
            crate::format_error_chain(&err),
            "connect errors must keep the plain error chain"
        );
    }

    #[tokio::test]
    async fn sse_chunk_stream_names_idle_bound_when_body_goes_silent() {
        use axum::{Router, response::IntoResponse, routing::get};
        use futures_util::StreamExt as _;

        let app = Router::new().route(
            "/stream",
            get(|| async {
                let first = futures_util::stream::once(async {
                    Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(
                        b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
                    ))
                });
                let open = futures_util::stream::pending::<
                    Result<axum::body::Bytes, std::convert::Infallible>,
                >();
                axum::body::Body::from_stream(first.chain(open)).into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        // A 1 s read-idle bound gives the header exchange and first chunk a
        // comfortable window on a slow runner; the silent body still trips it
        // in about a second, keeping the whole test under ~2 s.
        let client = reqwest::Client::builder()
            .read_timeout(std::time::Duration::from_secs(1))
            .build()
            .unwrap();
        let response = client
            .get(format!("http://{addr}/stream"))
            .send()
            .await
            .unwrap();
        let mut stream = sse_bytes_to_chunks(
            response,
            false,
            crate::StreamIdleBound::Fixed(std::time::Duration::from_secs(300)),
        );
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .expect("first chunk must arrive before the stall")
            .expect("chunk stream must yield a chunk")
            .expect("first chunk must be valid");
        assert_eq!(first.delta, "hi");
        let item = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .expect("stall must hit the read-idle bound")
            .expect("stalled chunk stream must yield an item");
        server.abort();
        let message = match item {
            Err(StreamError::Http(message)) => message,
            Err(other) => panic!("expected an HTTP stream error, got {other:?}"),
            Ok(_) => panic!("expected an error after the stalled body"),
        };
        assert!(
            message.contains("no data from provider for 300s"),
            "idle message must name the bound that fired: {message}"
        );
        assert!(
            message.contains("stream idle timeout"),
            "idle message must name the idle timeout: {message}"
        );
        assert!(
            !message.contains("timeout_secs"),
            "a fixed idle bound has no knob advice: {message}"
        );
    }

    #[test]
    fn timeout_secs_overrides_default() {
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .timeout_secs(300)
            .build();
        assert_eq!(p.timeout_secs, 300);
    }

    #[test]
    fn extra_headers_default_empty() {
        let p = make_model_provider("test", "https://example.com", None);
        assert!(p.extra_headers.is_empty());
    }

    #[test]
    fn extra_headers_sets_headers() {
        let mut headers = std::collections::HashMap::new();
        headers.insert("X-Title".to_string(), "zeroclaw".to_string());
        headers.insert(
            "HTTP-Referer".to_string(),
            "https://example.com".to_string(),
        );
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .extra_headers(headers)
            .build();
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
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .extra_headers(headers)
            .build();
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
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .user_agent("CustomAgent/1.0")
            .extra_headers(headers)
            .build();
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
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("MiniMax")
            .base_url("https://api.minimax.chat/v1")
            .credential(Some("k"))
            .auth_style(AuthStyle::Bearer)
            .merge_system_into_user()
            .build();
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

    #[test]
    fn strip_native_tool_messages_coalesces_adjacent_users() {
        let messages = vec![
            ChatMessage::user("summarize this build output"),
            ChatMessage::assistant(
                r#"{"content":"","tool_calls":[{"id":"t1","name":"shell","arguments":"{}"}]}"#,
            ),
            ChatMessage::tool(r#"{"tool_call_id":"t1","content":"cargo output"}"#),
            ChatMessage::user("go on"),
        ];
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("Anthropic-compatible")
            .base_url("https://example.test/v1")
            .credential(Some("k"))
            .auth_style(AuthStyle::Bearer)
            .merge_system_into_user()
            .build();
        let stripped = p.strip_native_tool_messages(&messages);
        let roles: Vec<&str> = stripped.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["user"]);
        assert!(
            stripped[0].content.contains("summarize this build output")
                && stripped[0].content.contains("go on"),
            "merged user message should preserve the original prompt and continuation; got {:?}",
            stripped[0].content
        );
    }

    #[test]
    fn strip_native_tool_messages_drops_internal_pruning_markers_before_coalescing() {
        let messages = vec![
            ChatMessage {
                role: "assistant".to_string(),
                content: ChatMessage::pruned_tool_exchange_summary(1),
            },
            ChatMessage::pruned_context_separator(),
            ChatMessage::assistant(
                r#"{"content":"","tool_calls":[{"id":"t1","name":"shell","arguments":"{}"}]}"#,
            ),
            ChatMessage::tool(r#"{"tool_call_id":"t1","content":"cargo output"}"#),
            ChatMessage::user("go on"),
        ];
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("Anthropic-compatible")
            .base_url("https://example.test/v1")
            .credential(Some("k"))
            .auth_style(AuthStyle::Bearer)
            .merge_system_into_user()
            .build();
        let stripped = p.strip_native_tool_messages(&messages);
        let roles: Vec<&str> = stripped.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["user"]);
        assert_eq!(stripped[0].content, "go on");
    }

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
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("MiniMax")
            .base_url("https://api.minimax.chat/v1")
            .credential(Some("k"))
            .auth_style(AuthStyle::Bearer)
            .merge_system_into_user()
            .build();
        let stripped = p.strip_native_tool_messages(&messages);
        assert_eq!(
            stripped.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
            vec!["user", "assistant"]
        );
        assert_eq!(stripped[1].content, "Here are the results");
    }

    #[tokio::test]
    async fn stream_chat_with_tools_sends_typed_tools_in_streaming_body() {
        use axum::response::IntoResponse;
        use axum::{Json, Router, routing::post};
        use futures_util::StreamExt as _;
        use std::sync::Mutex;
        use tokio::net::TcpListener;

        // Pins the stream_chat call-site wiring into
        // build_streaming_native_tool_request (helper-level tests alone
        // would not catch e.g. swapped bool arguments or tools dropped at
        // the call site).
        let captured: std::sync::Arc<Mutex<Option<serde_json::Value>>> =
            std::sync::Arc::new(Mutex::new(None));
        let captured_clone = captured.clone();

        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(body): Json<serde_json::Value>| {
                let cap = captured_clone.clone();
                async move {
                    *cap.lock().unwrap() = Some(body);
                    (
                        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                        "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n",
                    )
                        .into_response()
                }
            }),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_handle = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let provider = make_model_provider("vllm", &format!("http://{addr}"), Some("key"));
        let tools = vec![zeroclaw_api::tool::ToolSpec::new(
            "get_weather",
            "Fetch the weather",
            serde_json::json!({ "type": "object", "properties": {} }),
        )];

        let mut stream = provider.stream_chat(
            crate::traits::ChatRequest {
                messages: &[ChatMessage::user("hi")],
                tools: Some(&tools),
                thinking: None,
            },
            "test-model",
            None,
            StreamOptions {
                enabled: true,
                count_tokens: false,
            },
        );
        while stream.next().await.is_some() {}

        let body = captured
            .lock()
            .unwrap()
            .take()
            .expect("no streaming request captured");
        assert_eq!(body["stream"], serde_json::json!(true));
        assert_eq!(body["tool_choice"], serde_json::json!("auto"));
        assert_eq!(
            body["tools"],
            serde_json::json!([{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Fetch the weather",
                    "parameters": { "type": "object", "properties": {} }
                }
            }]),
            "streaming request body must carry the converted typed tools"
        );

        server_handle.abort();
    }

    #[tokio::test]
    async fn chat_with_tools_forwards_raw_specs_without_validation_or_sanitizing() {
        use axum::{Json, Router, routing::post};
        use std::sync::Mutex;
        use tokio::net::TcpListener;

        let captured: std::sync::Arc<Mutex<Option<serde_json::Value>>> =
            std::sync::Arc::new(Mutex::new(None));
        let captured_clone = captured.clone();

        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(body): Json<serde_json::Value>| {
                let cap = captured_clone.clone();
                async move {
                    *cap.lock().unwrap() = Some(body);
                    Json(serde_json::json!({
                        "id": "chatcmpl-test",
                        "choices": [{
                            "index": 0,
                            "message": { "role": "assistant", "content": "ok" },
                            "finish_reason": "stop"
                        }],
                        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
                    }))
                }
            }),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_handle = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let provider = OpenAiCompatibleModelProvider::builder("lmstudio")
            .display_name("lmstudio")
            .base_url(&format!("http://{addr}"))
            .credential(Some("key"))
            .auth_style(AuthStyle::Bearer)
            .local_model_tool_sanitize()
            .build();
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![
            // OpenAI permits both description and parameters to be omitted.
            serde_json::json!({
                "type": "function",
                "function": { "name": "get_weather" }
            }),
            // Raw callers historically controlled vendor extensions and
            // schema shape. Even with local sanitization configured, this
            // entry must not be parsed or cleaned in this allocation-only PR.
            serde_json::json!({
                "type": "vendor_extension",
                "function": {
                    "name": "lookup",
                    "parameters": {
                        "$defs": { "Id": { "type": "string" } },
                        "additionalProperties": false
                    }
                },
                "x_vendor_hint": "keep-me"
            }),
        ];

        let result = provider
            .chat_with_tools(&messages, &tools, "gemma-4-9b-it", None)
            .await;
        assert!(
            result.is_ok(),
            "raw compatible-provider specs must be forwarded: {:?}",
            result.err()
        );

        let body = captured
            .lock()
            .unwrap()
            .take()
            .expect("no request captured by mock server");
        assert_eq!(
            body["tools"],
            serde_json::json!(tools),
            "raw tools must reach the request body byte-shape-equivalent, \
             including optional-field omissions and sanitizer-sensitive keys"
        );
        assert_eq!(
            body["tool_choice"],
            serde_json::json!("auto"),
            "tool_choice must be auto when tools are present"
        );

        server_handle.abort();
    }

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

        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url(&format!("http://{addr}"))
            .credential(Some("k"))
            .auth_style(AuthStyle::Bearer)
            .build();

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

    fn minimal_request(temperature: Option<f64>) -> ApiChatRequest {
        ApiChatRequest {
            model: "any-model".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: MessageContent::Text("hi".to_string()),
                thinking_blocks: None,
            }],
            temperature,
            stream: None,
            stream_options: None,
            reasoning_effort: None,
            tool_stream: None,
            tools: None,
            tool_choice: None,
            max_tokens: None,
            extra_body: None,
        }
    }

    #[test]
    fn unset_temperature_is_omitted_from_wire() {
        // `None` must honor the `Option<f64>` contract: no `temperature` field
        // on the wire, regardless of model name. Generalizes the former
        // kimi-k2-only special case
        let body = serde_json::to_value(minimal_request(None)).unwrap();
        assert!(
            body.get("temperature").is_none(),
            "unset temperature must be omitted from the request body, got: {body}"
        );
    }

    #[test]
    fn explicit_temperature_is_sent_on_wire() {
        let body = serde_json::to_value(minimal_request(Some(0.5))).unwrap();
        assert_eq!(
            body.get("temperature").and_then(|v| v.as_f64()),
            Some(0.5),
            "explicit temperature must be sent verbatim, got: {body}"
        );
    }

    #[test]
    fn models_dev_to_model_info_returns_no_pricing() {
        // The models.dev catalog does not serve pricing data; every entry
        // must have `pricing: None`. This documents the intentional contract.
        let ids = vec![
            ("openai/gpt-4o".to_string(), None),
            ("anthropic/claude-sonnet-4-6".to_string(), None),
        ];
        let models = models_dev_to_model_info(ids);
        assert_eq!(models.len(), 2);
        // Preserves input order (no sorting — caller decides).
        assert_eq!(models[0].id, "openai/gpt-4o");
        assert!(models[0].pricing.is_none());
        assert_eq!(models[1].id, "anthropic/claude-sonnet-4-6");
        assert!(models[1].pricing.is_none());
    }

    #[test]
    fn models_dev_to_model_info_carries_context_window() {
        // The catalog's `limit.context` must survive the mapping, and a model
        // the catalog gives no limit for must stay `None` — not a stub value.
        let ids = vec![
            ("anthropic/claude-opus-4-8".to_string(), Some(1_000_000)),
            ("some/unknown-model".to_string(), None),
        ];
        let models = models_dev_to_model_info(ids);
        assert_eq!(models[0].context_window, Some(1_000_000));
        assert_eq!(models[1].context_window, None);
    }

    #[test]
    fn public_model_listing_flag_defaults_false() {
        // Providers without explicit public_model_listing must default to false,
        // preserving existing behavior for all established providers.
        let p = make_model_provider("test", "https://example.com", None);
        assert!(!p.public_model_listing);
    }

    #[test]
    fn public_model_listing_flag_can_be_set() {
        // Verify the builder correctly enables public_model_listing.
        let p = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .public_model_listing()
            .build();
        assert!(p.public_model_listing);
    }

    #[test]
    fn token_count_u64_positive() {
        assert_eq!(normalize_token_count_value(serde_json::json!(42)), Some(42));
    }

    #[test]
    fn token_count_u64_zero() {
        assert_eq!(normalize_token_count_value(serde_json::json!(0)), Some(0));
    }

    #[test]
    fn token_count_large_u64() {
        assert_eq!(
            normalize_token_count_value(serde_json::json!(u64::MAX)),
            Some(u64::MAX)
        );
    }

    #[test]
    fn token_count_i64_positive() {
        assert_eq!(
            normalize_token_count_value(serde_json::json!(100i64)),
            Some(100)
        );
    }

    #[test]
    fn token_count_i64_negative() {
        assert_eq!(
            normalize_token_count_value(serde_json::json!(-1i64)),
            None,
            "negative token counts must be rejected"
        );
    }

    #[test]
    fn token_count_f64_positive_integer() {
        assert_eq!(
            normalize_token_count_value(serde_json::json!(15.0)),
            Some(15)
        );
    }

    #[test]
    fn token_count_f64_fractional() {
        assert_eq!(
            normalize_token_count_value(serde_json::json!(3.7)),
            Some(3),
            "fractional floats floor toward zero"
        );
    }

    #[test]
    fn token_count_f64_less_than_one() {
        assert_eq!(
            normalize_token_count_value(serde_json::json!(0.5)),
            Some(0),
            "fractional token < 1 counts as zero (avoids noise)"
        );
    }

    #[test]
    fn token_count_f64_negative() {
        assert_eq!(
            normalize_token_count_value(serde_json::json!(-0.5)),
            None,
            "negative float token counts must be rejected"
        );
    }

    #[test]
    fn token_count_f64_nan() {
        let nan: f64 = f64::NAN;
        assert_eq!(
            normalize_token_count_value(serde_json::json!(nan)),
            None,
            "NaN must be rejected"
        );
    }

    #[test]
    fn token_count_f64_infinity() {
        assert_eq!(
            normalize_token_count_value(serde_json::json!(f64::INFINITY)),
            None,
            "+Infinity must be rejected"
        );
    }

    #[test]
    fn token_count_f64_neg_infinity() {
        assert_eq!(
            normalize_token_count_value(serde_json::json!(f64::NEG_INFINITY)),
            None,
            "-Infinity must be rejected"
        );
    }

    #[test]
    fn token_count_f64_exceeds_u64_max() {
        assert_eq!(
            normalize_token_count_value(serde_json::json!(u64::MAX as f64 * 2.0)),
            None,
            "value > u64::MAX must be rejected"
        );
    }

    #[test]
    fn token_count_string_integer() {
        assert_eq!(
            normalize_token_count_value(serde_json::json!("15")),
            Some(15)
        );
    }

    #[test]
    fn token_count_string_float() {
        assert_eq!(
            normalize_token_count_value(serde_json::json!("3.7")),
            Some(3)
        );
    }

    #[test]
    fn token_count_string_whitespace() {
        assert_eq!(
            normalize_token_count_value(serde_json::json!(" 20 ")),
            Some(20)
        );
    }

    #[test]
    fn token_count_string_negative() {
        assert_eq!(normalize_token_count_value(serde_json::json!("-5")), None);
    }

    #[test]
    fn token_count_string_garbage() {
        assert_eq!(
            normalize_token_count_value(serde_json::json!("not-a-number")),
            None
        );
    }

    #[test]
    fn token_count_null() {
        assert_eq!(normalize_token_count_value(serde_json::Value::Null), None);
    }

    #[test]
    fn token_count_bool() {
        assert_eq!(
            normalize_token_count_value(serde_json::json!(true)),
            None,
            "boolean must not be misinterpreted as token count"
        );
    }

    #[test]
    fn token_count_array() {
        assert_eq!(
            normalize_token_count_value(serde_json::json!([1, 2, 3])),
            None
        );
    }

    #[test]
    fn token_count_object() {
        assert_eq!(
            normalize_token_count_value(serde_json::json!({"count": 10})),
            None
        );
    }

    // ── `deserialize_optional_token_count` round-trip tests ────────────
    // Validate the full deserialize path through a UsageInfo-shaped struct
    // so the serde attribute wiring is exercised as well.

    #[derive(Debug, Deserialize)]
    struct TestUsage {
        #[serde(default, deserialize_with = "deserialize_optional_token_count")]
        prompt_cache_hit_tokens: Option<u64>,
        #[serde(default, deserialize_with = "deserialize_optional_token_count")]
        prompt_tokens: Option<u64>,
    }

    #[test]
    fn deserialize_token_count_integer() {
        let usage: TestUsage =
            serde_json::from_str(r#"{"prompt_tokens": 5000, "prompt_cache_hit_tokens": 3000}"#)
                .unwrap();
        assert_eq!(usage.prompt_tokens, Some(5000));
        assert_eq!(usage.prompt_cache_hit_tokens, Some(3000));
    }

    #[test]
    fn deserialize_token_count_float() {
        let usage: TestUsage =
            serde_json::from_str(r#"{"prompt_tokens": 12.8, "prompt_cache_hit_tokens": 100.3}"#)
                .unwrap();
        assert_eq!(usage.prompt_tokens, Some(12));
        assert_eq!(usage.prompt_cache_hit_tokens, Some(100));
    }

    #[test]
    fn deserialize_token_count_string() {
        let usage: TestUsage =
            serde_json::from_str(r#"{"prompt_tokens": "1000", "prompt_cache_hit_tokens": "500"}"#)
                .unwrap();
        assert_eq!(usage.prompt_tokens, Some(1000));
        assert_eq!(usage.prompt_cache_hit_tokens, Some(500));
    }

    #[test]
    fn deserialize_token_count_null() {
        let usage: TestUsage =
            serde_json::from_str(r#"{"prompt_tokens": null, "prompt_cache_hit_tokens": null}"#)
                .unwrap();
        assert_eq!(usage.prompt_tokens, None);
        assert_eq!(usage.prompt_cache_hit_tokens, None);
    }

    #[test]
    fn deserialize_token_count_negative() {
        let usage: TestUsage =
            serde_json::from_str(r#"{"prompt_tokens": -1, "prompt_cache_hit_tokens": 0}"#).unwrap();
        assert_eq!(
            usage.prompt_tokens, None,
            "negative values must be rejected"
        );
        assert_eq!(usage.prompt_cache_hit_tokens, Some(0));
    }

    #[test]
    fn deserialize_token_count_missing_field() {
        let usage: TestUsage = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(usage.prompt_tokens, None);
        assert_eq!(usage.prompt_cache_hit_tokens, None);
    }

    // ── `UsageInfo::cached_input_tokens` priority tests ─────────────────
    // `prompt_cache_hit_tokens` (DeepSeek-style) takes priority over
    // `prompt_tokens_details.cached_tokens` (OpenAI-style).

    #[test]
    fn usageinfo_cached_input_prefers_prompt_cache_hit_tokens() {
        let json = serde_json::json!({
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "prompt_cache_hit_tokens": 60,
            "prompt_tokens_details": {"cached_tokens": 20}
        });
        let usage: UsageInfo = serde_json::from_value(json).unwrap();
        assert_eq!(usage.cached_input_tokens(), Some(60));
    }

    #[test]
    fn usageinfo_cached_input_falls_back_to_details() {
        let json = serde_json::json!({
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "prompt_tokens_details": {"cached_tokens": 20}
        });
        let usage: UsageInfo = serde_json::from_value(json).unwrap();
        assert_eq!(usage.cached_input_tokens(), Some(20));
    }

    #[test]
    fn usageinfo_cached_input_returns_none_when_absent() {
        let json = serde_json::json!({
            "prompt_tokens": 100,
            "completion_tokens": 50
        });
        let usage: UsageInfo = serde_json::from_value(json).unwrap();
        assert_eq!(usage.cached_input_tokens(), None);
    }

    #[test]
    fn usageinfo_into_provider_usage_forwards_cached_tokens() {
        let json = serde_json::json!({
            "prompt_tokens": 1000,
            "completion_tokens": 200,
            "prompt_cache_hit_tokens": 400
        });
        let usage: UsageInfo = serde_json::from_value(json).unwrap();
        let out = usage.into_provider_usage();
        assert_eq!(out.input_tokens, Some(1000));
        assert_eq!(out.output_tokens, Some(200));
        assert_eq!(out.cached_input_tokens, Some(400));
    }

    #[test]
    fn convert_messages_for_native_strips_reasoning_when_replay_disabled() {
        let provider = OpenAiCompatibleModelProvider::builder("test")
            .display_name("test")
            .base_url("https://example.com")
            .credential(None)
            .auth_style(AuthStyle::Bearer)
            .without_assistant_reasoning_replay()
            .build();
        let messages = vec![ChatMessage::assistant(
            r#"{"content":"ok","reasoning_content":"step 1"}"#.to_string(),
        )];
        let native = provider.convert_messages_for_native(&messages, true);
        assert_eq!(native.len(), 1);
        assert_eq!(native[0].role, "assistant");
        assert_eq!(native[0].reasoning_content, None);
        assert_eq!(native[0].reasoning, None);
    }

    #[test]
    fn convert_messages_for_native_tool_fallbacks_to_last_assistant_tool_call_id() {
        let provider = make_model_provider("test", "https://example.com", None);
        let messages = vec![
            ChatMessage::assistant(
                r#"{"content":null,"tool_calls":[{"id":"fc_123","name":"search","arguments":"{}"}]}"#.to_string(),
            ),
            ChatMessage::tool(
                r#"{"content":"result"}"#.to_string(), // missing tool_call_id
            ),
        ];
        let native = provider.convert_messages_for_native(&messages, true);
        assert_eq!(native.len(), 2);
        assert_eq!(native[1].role, "tool");
        assert_eq!(native[1].tool_call_id.as_deref(), Some("fc_123"));
    }

    #[test]
    fn convert_messages_for_native_tool_uses_explicit_id_when_present() {
        let provider = make_model_provider("test", "https://example.com", None);
        let messages = vec![
            ChatMessage::assistant(
                r#"{"content":null,"tool_calls":[{"id":"fc_123","name":"search","arguments":"{}"}]}"#.to_string(),
            ),
            ChatMessage::tool(
                r#"{"tool_call_id":"fc_456","content":"result"}"#.to_string(),
            ),
        ];
        let native = provider.convert_messages_for_native(&messages, true);
        assert_eq!(native.len(), 2);
        assert_eq!(native[1].role, "tool");
        assert_eq!(native[1].tool_call_id.as_deref(), Some("fc_456"));
    }

    /// A profile that authenticates purely through `extra_headers` (a
    /// `Cookie`- or `X-Auth`-style bridge, rather than a credential
    /// `resolve_credential()` returns) must still probe the configured
    /// endpoint's `/models`, and a real failure there must be surfaced —
    /// not silently swapped for an unrelated models.dev/OpenRouter catalog.
    #[tokio::test]
    async fn list_models_probes_configured_endpoint_for_cookie_only_auth() {
        use axum::Router;
        use axum::extract::State;
        use axum::http::HeaderMap;
        use axum::routing::get;
        use std::sync::{Arc, Mutex};
        use tokio::net::TcpListener;

        let captured: Arc<Mutex<Option<HeaderMap>>> = Arc::new(Mutex::new(None));
        let captured_for_route = captured.clone();
        let app = Router::new().route(
            "/models",
            get(
                move |headers: HeaderMap, State(capture): State<Arc<Mutex<Option<HeaderMap>>>>| {
                    *capture.lock().unwrap() = Some(headers);
                    async move { axum::http::StatusCode::UNAUTHORIZED }
                },
            )
            .with_state(captured_for_route),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_handle = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let mut headers = std::collections::HashMap::new();
        headers.insert("Cookie".to_string(), "session=abc123".to_string());
        let provider = OpenAiCompatibleModelProvider::builder("cookie-auth")
            .display_name("cookie-auth")
            .base_url(&format!("http://{addr}"))
            .auth_style(AuthStyle::Bearer)
            .extra_headers(headers)
            .build();

        let error = provider
            .list_models()
            .await
            .expect_err("a Cookie-only profile's real endpoint failure must be surfaced");
        assert!(
            error.to_string().contains("HTTP 401"),
            "expected the configured endpoint's actual failure, got: {error}"
        );
        assert!(
            captured.lock().unwrap().is_some(),
            "the configured endpoint must actually be probed for a header-only auth profile"
        );

        server_handle.abort();
    }

    #[tokio::test]
    async fn list_models_probes_configured_endpoint_for_x_auth_only_auth() {
        use axum::Router;
        use axum::routing::get;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        use tokio::net::TcpListener;

        let requests = Arc::new(AtomicUsize::new(0));
        let requests_for_route = requests.clone();
        let app = Router::new().route(
            "/models",
            get(move || {
                let requests = requests_for_route.clone();
                async move {
                    requests.fetch_add(1, Ordering::SeqCst);
                    axum::http::StatusCode::FORBIDDEN
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_handle = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let mut headers = std::collections::HashMap::new();
        headers.insert("X-Auth".to_string(), "bridge-token".to_string());
        let provider = OpenAiCompatibleModelProvider::builder("x-auth-only")
            .display_name("x-auth-only")
            .base_url(&format!("http://{addr}"))
            .auth_style(AuthStyle::Bearer)
            .extra_headers(headers)
            .build();

        let error = provider
            .list_models_with_pricing()
            .await
            .expect_err("an X-Auth-only profile's real endpoint failure must be surfaced");
        assert!(
            error.to_string().contains("HTTP 403"),
            "expected the configured endpoint's actual failure, got: {error}"
        );
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "the configured endpoint must be probed exactly once for a header-only auth profile"
        );

        let _unused: Arc<Mutex<()>> = Arc::new(Mutex::new(()));
        server_handle.abort();
    }

    /// A configured endpoint URL may carry credentials in its userinfo,
    /// query, or fragment. The header-only probe branch reports transport
    /// failures before the central catalog caller sanitizes the returned
    /// error, so the URL it embeds must already be scrubbed.
    #[tokio::test]
    async fn list_models_scrubs_url_credentials_from_transport_failure() {
        // Bind and immediately drop the listener so the port is closed: this
        // forces a connect-level transport failure (the `map_err` branch that
        // formats the URL), rather than an HTTP status failure.
        let closed_addr = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap()
        };

        let mut headers = std::collections::HashMap::new();
        headers.insert("X-Auth".to_string(), "bridge-token".to_string());
        let provider = OpenAiCompatibleModelProvider::builder("url-credential")
            .display_name("url-credential")
            .base_url(&format!(
                "http://synthetic-user:synthetic-secret@{}:{}",
                closed_addr.ip(),
                closed_addr.port()
            ))
            .auth_style(AuthStyle::Bearer)
            .extra_headers(headers)
            .build();

        let error = provider
            .list_models()
            .await
            .expect_err("a closed configured endpoint must surface a transport failure");
        let rendered = format!("{error:#}");
        assert!(
            !rendered.contains("synthetic-secret"),
            "URL userinfo credentials must not reach the returned error: {rendered}"
        );
        assert!(
            !rendered.contains("synthetic-user"),
            "URL userinfo must not reach the returned error: {rendered}"
        );
        assert!(
            rendered.contains("[REDACTED]"),
            "the scrubbed URL should retain a redaction marker: {rendered}"
        );
    }
}
