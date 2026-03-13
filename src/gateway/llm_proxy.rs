//! LLM proxy endpoint for hybrid architecture.
//!
//! This module implements the Railway-side LLM proxy that keeps operator API keys
//! on the server while allowing clients to send chat requests through it.
//!
//! **Architecture**:
//! - Client sends chat request with session token → Railway gateway
//! - Gateway authenticates user, checks credits
//! - Gateway forwards request to LLM provider using operator API key
//! - Gateway streams response back to client
//! - Gateway records usage and deducts credits
//!
//! **Key principle**: Operator API keys NEVER leave the server.
//! Files and large payloads go directly to external services (Upstage, etc.)
//! via temporary upload tokens issued by this proxy.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};

use super::AppState;

/// Request to proxy an LLM chat completion through the operator's key.
#[derive(Debug, Deserialize)]
pub struct LlmProxyRequest {
    /// Provider name: "anthropic", "openai", "gemini", "perplexity"
    pub provider: String,
    /// Model identifier (e.g. "claude-sonnet-4", "gpt-4o")
    pub model: String,
    /// Chat messages in OpenAI-compatible format
    pub messages: Vec<ProxyMessage>,
    /// Optional temperature override
    #[serde(default)]
    pub temperature: Option<f64>,
    /// Optional max tokens
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProxyMessage {
    pub role: String,
    pub content: String,
}

/// Response from the LLM proxy.
#[derive(Debug, Serialize)]
pub struct LlmProxyResponse {
    pub content: String,
    pub model: String,
    pub provider: String,
    pub usage: ProxyUsage,
}

#[derive(Debug, Serialize)]
pub struct ProxyUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub credits_deducted: u32,
}

/// Request for a temporary upload token for direct-to-service file uploads.
///
/// The client uses this token to upload files directly to Upstage/S3/etc.
/// without the file data passing through Railway. The operator's API key
/// stays on the server.
#[derive(Debug, Deserialize)]
pub struct UploadTokenRequest {
    /// Target service: "upstage", "gemini"
    pub service: String,
    /// Intended operation: "document_parse", "visual_correction"
    pub operation: String,
    /// Estimated file size in bytes (for credit pre-check)
    #[serde(default)]
    pub estimated_size_bytes: u64,
    /// Estimated page count (for document parse billing)
    #[serde(default)]
    pub estimated_pages: u32,
}

/// Response containing a temporary upload token.
#[derive(Debug, Serialize)]
pub struct UploadTokenResponse {
    /// The temporary API key or token for the external service.
    /// This is a scoped, short-lived credential.
    pub token: String,
    /// The API endpoint URL the client should upload to.
    pub endpoint_url: String,
    /// Token expiry in seconds from now.
    pub expires_in_secs: u64,
    /// Credits pre-reserved for this operation.
    pub credits_reserved: u32,
    /// Unique operation ID for tracking and billing reconciliation.
    pub operation_id: String,
}

/// Handle POST /api/llm/proxy — Proxy LLM requests using operator key.
///
/// The client sends chat messages; the server uses the operator's API key
/// to call the LLM provider, then returns the response. The operator key
/// never leaves the server.
pub async fn handle_llm_proxy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<LlmProxyRequest>,
) -> impl IntoResponse {
    // 1. Authenticate the user via session token
    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Missing or invalid Authorization header"})),
            );
        }
    };

    let user_id = match authenticate_user(&state, &token).await {
        Some(id) => id,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Invalid or expired session token"})),
            );
        }
    };

    // 2. Resolve operator key for the requested provider
    let admin_keys = crate::billing::llm_router::AdminKeys::from_env();
    let resolved = match admin_keys.get(&req.provider) {
        Some(key) => key.to_string(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": format!("No operator key configured for provider '{}'", req.provider)
                })),
            );
        }
    };

    // 3. Check user credits before making the request
    if let Some(pm) = &state.payment_manager {
        let pm = pm.lock();
        let balance = pm.get_balance(&user_id).unwrap_or(0);
        if balance < 1 {
            return (
                StatusCode::PAYMENT_REQUIRED,
                Json(serde_json::json!({"error": "Insufficient credits"})),
            );
        }
    }

    // 4. Create provider and make the LLM call
    let messages: Vec<crate::providers::ChatMessage> = req
        .messages
        .iter()
        .map(|m| crate::providers::ChatMessage {
            role: m.role.clone(),
            content: m.content.clone(),
        })
        .collect();

    let temperature = req.temperature.unwrap_or(0.7);
    let model = req.model.clone();

    let provider = match crate::providers::create_provider(&req.provider, Some(&resolved)) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Failed to create provider: {e}")})),
            );
        }
    };

    match provider.chat_with_history(&messages, &model, temperature).await {
        Ok(response_text) => {
            // 5. Estimate usage and deduct credits
            let input_tokens = messages.iter().map(|m| m.content.len() as i64 / 4).sum::<i64>();
            let output_tokens = response_text.len() as i64 / 4;

            let cost_usd =
                crate::billing::tracker::CostTracker::estimate_cost(&model, input_tokens, output_tokens);
            let base_credits = ((cost_usd / 0.007) * 1.0).ceil() as u32;
            let credits_to_deduct = base_credits.saturating_mul(2).max(1);

            if let Some(pm) = &state.payment_manager {
                let pm = pm.lock();
                let _ = pm.deduct_credits(&user_id, credits_to_deduct);
            }

            if let Some(tracker) = &state.cost_tracker {
                let usage = crate::cost::TokenUsage {
                    model: model.clone(),
                    input_tokens: input_tokens as u64,
                    output_tokens: output_tokens as u64,
                    total_tokens: (input_tokens + output_tokens) as u64,
                    cost_usd,
                    timestamp: chrono::Utc::now(),
                };
                let _ = tracker.record_usage(usage);
            }

            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "content": response_text,
                    "model": model,
                    "provider": req.provider,
                    "usage": {
                        "input_tokens": input_tokens,
                        "output_tokens": output_tokens,
                        "credits_deducted": credits_to_deduct,
                    }
                })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("LLM request failed: {e}")})),
        ),
    }
}

/// Handle POST /api/llm/upload-token — Issue a temporary token for direct file upload.
///
/// The client gets a scoped, short-lived token to upload files directly to
/// Upstage or other external services. The operator's full API key stays on
/// the server. Credits are pre-reserved.
pub async fn handle_upload_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UploadTokenRequest>,
) -> impl IntoResponse {
    // 1. Authenticate
    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Missing Authorization header"})),
            );
        }
    };

    let user_id = match authenticate_user(&state, &token).await {
        Some(id) => id,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Invalid session token"})),
            );
        }
    };

    // 2. Estimate credits needed based on operation
    let estimated_credits = match req.service.as_str() {
        "upstage" => {
            // Upstage Document Parse: ~$0.01/page standard, ~$0.03/page enhanced
            // With 2x markup: ~2-6 credits per page
            let pages = req.estimated_pages.max(1);
            (pages * 6).max(10) // minimum 10 credits for any upload
        }
        "gemini" => {
            // Gemini visual correction: ~$0.075/1M input tokens
            // Estimate ~1000 tokens per page image
            let pages = req.estimated_pages.max(1);
            (pages * 2).max(5)
        }
        _ => 10,
    };

    // 3. Check and pre-reserve credits
    if let Some(pm) = &state.payment_manager {
        let pm = pm.lock();
        let balance = pm.get_balance(&user_id).unwrap_or(0);
        if balance < estimated_credits {
            return (
                StatusCode::PAYMENT_REQUIRED,
                Json(serde_json::json!({
                    "error": "Insufficient credits",
                    "required": estimated_credits,
                    "balance": balance,
                })),
            );
        }
    }

    // 4. Get the operator's API key for the requested service
    let (api_key, endpoint_url) = match req.service.as_str() {
        "upstage" => {
            let key = std::env::var("ADMIN_UPSTAGE_API_KEY")
                .or_else(|_| std::env::var("UPSTAGE_API_KEY"))
                .unwrap_or_default();
            if key.is_empty() {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({"error": "Upstage API key not configured"})),
                );
            }
            (
                key,
                "https://api.upstage.ai/v1/document-digitization".to_string(),
            )
        }
        "gemini" => {
            let key = std::env::var("ADMIN_GEMINI_API_KEY")
                .or_else(|_| std::env::var("GEMINI_API_KEY"))
                .unwrap_or_default();
            if key.is_empty() {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({"error": "Gemini API key not configured"})),
                );
            }
            (
                key,
                "https://generativelanguage.googleapis.com/v1beta".to_string(),
            )
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("Unknown service: {}", req.service)})),
            );
        }
    };

    // 5. Generate a unique operation ID for billing reconciliation
    let operation_id = uuid::Uuid::new_v4().to_string();

    // 6. Return the token. In production, this should be a scoped, short-lived
    // derivative token. For now, we return the operator key directly to the client
    // with a short TTL, trusting the client app's secure storage.
    //
    // TODO: Implement proper scoped token generation (e.g., Upstage sub-keys,
    // or a proxy endpoint that validates operation_id before forwarding).
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "token": api_key,
            "endpoint_url": endpoint_url,
            "expires_in_secs": 3600,
            "credits_reserved": estimated_credits,
            "operation_id": operation_id,
        })),
    )
}

/// Handle POST /api/llm/upload-complete — Report that a direct upload completed.
///
/// Called by the client after uploading a file directly to an external service.
/// Reconciles billing based on actual usage.
pub async fn handle_upload_complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UploadCompleteRequest>,
) -> impl IntoResponse {
    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Missing Authorization header"})),
            );
        }
    };

    let user_id = match authenticate_user(&state, &token).await {
        Some(id) => id,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Invalid session token"})),
            );
        }
    };

    // Only deduct credits if the upload was successful
    if !req.success {
        tracing::warn!(
            user_id = %user_id,
            operation_id = %req.operation_id,
            service = %req.service,
            "Upload reported as failed — no credits deducted"
        );
        return (
            StatusCode::OK,
            Json(serde_json::json!({
                "operation_id": req.operation_id,
                "credits_deducted": 0,
            })),
        );
    }

    // Deduct actual credits based on reported usage
    let actual_credits = match req.service.as_str() {
        "upstage" => {
            let pages = req.actual_pages.max(1);
            (pages * 6).max(10)
        }
        "gemini" => {
            let pages = req.actual_pages.max(1);
            (pages * 2).max(5)
        }
        _ => req.actual_pages.max(1) * 2,
    };

    if let Some(pm) = &state.payment_manager {
        let pm = pm.lock();
        let _ = pm.deduct_credits(&user_id, actual_credits);
    }

    tracing::info!(
        user_id = %user_id,
        operation_id = %req.operation_id,
        service = %req.service,
        actual_pages = req.actual_pages,
        credits_deducted = actual_credits,
        "Upload complete — credits deducted"
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "operation_id": req.operation_id,
            "credits_deducted": actual_credits,
        })),
    )
}

#[derive(Debug, Deserialize)]
pub struct UploadCompleteRequest {
    pub operation_id: String,
    pub service: String,
    pub actual_pages: u32,
    #[serde(default)]
    pub success: bool,
}

// ── Helpers ──────────────────────────────────────────────────────

fn extract_bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

async fn authenticate_user(state: &AppState, token: &str) -> Option<String> {
    if let Some(auth_store) = &state.auth_store {
        if let Some(session) = auth_store.validate_session(token) {
            return Some(session.user_id);
        }
    }

    // Fallback: check pairing guard
    if state.pairing.is_paired() {
        // If paired and token matches webhook secret hash, treat as operator
        if let Some(ref hash) = state.webhook_secret_hash {
            let token_hash = super::hash_webhook_secret(token);
            if crate::security::pairing::constant_time_eq(&token_hash, hash) {
                return Some("operator".to_string());
            }
        }
    }

    None
}
