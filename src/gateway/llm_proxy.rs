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
//! Files and large payloads are uploaded to R2 via pre-signed URLs, then
//! processed server-side using the operator's keys.

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

// ── R2-based document upload flow ────────────────────────────────
//
// Secure image PDF processing: client uploads to R2 via pre-signed URL,
// Railway downloads from R2, calls Upstage with operator key.
// Operator API keys NEVER leave the server.

/// Request for a pre-signed R2 upload URL.
#[derive(Debug, Deserialize)]
pub struct DocumentUploadUrlRequest {
    /// Original filename (for extension detection and key generation).
    pub filename: String,
    /// MIME type (e.g. "application/pdf").
    pub content_type: String,
    /// Estimated page count (for credit pre-check).
    #[serde(default)]
    pub estimated_pages: u32,
}

/// Response with pre-signed upload URL.
#[derive(Debug, Serialize)]
pub struct DocumentUploadUrlResponse {
    /// Pre-signed PUT URL for direct upload to R2.
    pub upload_url: String,
    /// The R2 object key (used to reference the file later).
    pub object_key: String,
    /// URL expiry in seconds.
    pub expires_in_secs: u64,
}

/// Handle POST /api/document/upload-url — Generate a pre-signed R2 PUT URL.
///
/// The client uploads the file directly to R2 using this URL.
/// No file data passes through Railway. Operator keys stay on the server.
pub async fn handle_document_upload_url(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<DocumentUploadUrlRequest>,
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

    // 2. Check R2 configuration
    let r2 = match &state.r2_config {
        Some(r2) => r2,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "R2 storage not configured"})),
            );
        }
    };

    // 3. Pre-check credits (Upstage ≈ 6 credits/page, minimum 10)
    let estimated_credits = (req.estimated_pages.max(1) * 6).max(10);
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

    // 4. Generate object key and pre-signed PUT URL
    let object_key = crate::storage::r2::generate_object_key(&user_id, &req.filename);
    let expires_secs = 900; // 15 minutes
    let upload_url = r2.presigned_put_url(&object_key, &req.content_type, expires_secs);

    tracing::info!(
        user_id = %user_id,
        object_key = %object_key,
        "Generated R2 pre-signed upload URL"
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "upload_url": upload_url,
            "object_key": object_key,
            "expires_in_secs": expires_secs,
        })),
    )
}

/// Request to process a document already uploaded to R2.
#[derive(Debug, Deserialize)]
pub struct DocumentProcessR2Request {
    /// The R2 object key returned by /api/document/upload-url.
    pub object_key: String,
    /// Original filename (for extension detection).
    pub filename: String,
    /// Estimated page count (for billing).
    #[serde(default)]
    pub estimated_pages: u32,
}

/// Handle POST /api/document/process-r2 — Process a document from R2.
///
/// Flow: Railway downloads from R2 → calls Upstage with operator key →
/// returns HTML/Markdown to client → deletes temp file from R2.
///
/// Operator API keys NEVER leave the server.
pub async fn handle_document_process_r2(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<DocumentProcessR2Request>,
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

    // 2. Verify R2 is configured
    let r2 = match &state.r2_config {
        Some(r2) => r2.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "R2 storage not configured"})),
            );
        }
    };

    // 3. Validate object key belongs to this user (prevent unauthorized access)
    let expected_prefix = format!("documents/{}/", user_id);
    if !req.object_key.starts_with(&expected_prefix) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "Object key does not belong to this user"})),
        );
    }

    // 4. Get operator Upstage API key (stays on server)
    let admin_keys = crate::billing::llm_router::AdminKeys::from_env();
    let upstage_key = match admin_keys.get("upstage") {
        Some(key) => key.to_string(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "Upstage API key not configured on server"})),
            );
        }
    };

    // 5. Download file from R2
    tracing::info!(
        user_id = %user_id,
        object_key = %req.object_key,
        "Downloading document from R2 for processing"
    );

    let file_data = match r2.download_object(&req.object_key).await {
        Ok(data) => data,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Failed to download from R2: {e}")})),
            );
        }
    };

    // 6. Call Upstage Document Parse with operator key
    let client = reqwest::Client::new();
    // Derive MIME type from filename extension
    let mime_type = match req.filename.rsplit('.').next().map(|e| e.to_lowercase()).as_deref() {
        Some("pdf") => "application/pdf",
        Some("doc") => "application/msword",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("xls") => "application/vnd.ms-excel",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("ppt") => "application/vnd.ms-powerpoint",
        Some("pptx") => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        Some("hwp") | Some("hwpx") => "application/x-hwp",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        _ => "application/octet-stream",
    };
    let form = reqwest::multipart::Form::new()
        .part(
            "document",
            reqwest::multipart::Part::bytes(file_data)
                .file_name(req.filename.clone())
                .mime_str(mime_type)
                .unwrap_or_else(|_| {
                    reqwest::multipart::Part::bytes(Vec::new())
                }),
        )
        .text("model", "document-parse")
        .text("ocr", "force")
        .text("output_formats", "[\"html\"]")
        .text("coordinates", "true");

    let upstage_resp = client
        .post("https://api.upstage.ai/v1/document-digitization")
        .header("Authorization", format!("Bearer {upstage_key}"))
        .multipart(form)
        .timeout(std::time::Duration::from_secs(300))
        .send()
        .await;

    let upstage_resp = match upstage_resp {
        Ok(r) => r,
        Err(e) => {
            // Clean up R2 object on failure
            let _ = r2.delete_object(&req.object_key).await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Upstage API request failed: {e}")})),
            );
        }
    };

    if !upstage_resp.status().is_success() {
        let status = upstage_resp.status();
        let body = upstage_resp.text().await.unwrap_or_default();
        let _ = r2.delete_object(&req.object_key).await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Upstage API error (HTTP {status}): {body}")})),
        );
    }

    let data: serde_json::Value = match upstage_resp.json().await {
        Ok(d) => d,
        Err(e) => {
            let _ = r2.delete_object(&req.object_key).await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Failed to parse Upstage response: {e}")})),
            );
        }
    };

    // 7. Extract HTML from Upstage response
    let html = data
        .get("content")
        .and_then(|c| c.get("html"))
        .and_then(|h| h.as_str())
        .unwrap_or("")
        .to_string();

    let page_count = data
        .get("elements")
        .and_then(|e| e.as_array())
        .map(|elements| {
            elements
                .iter()
                .filter_map(|e| e.get("page").and_then(|p| p.as_u64()))
                .max()
                .unwrap_or(1) as u32
        })
        .unwrap_or(1);

    // 8. LLM correction is NOT done server-side.
    //    If the user has their own LLM API key, correction happens on the
    //    user's local MoA app. If not, the raw Upstage output is used as-is.

    // 9. Convert to markdown
    let markdown = crate::tools::document_pipeline::html_to_markdown_public(&html);

    // 10. Deduct credits
    let actual_credits = (page_count * 6).max(10);
    if let Some(pm) = &state.payment_manager {
        let pm = pm.lock();
        let _ = pm.deduct_credits(&user_id, actual_credits);
    }

    // 11. Clean up R2 object
    let object_key = req.object_key.clone();
    let r2_cleanup = r2;
    tokio::spawn(async move {
        if let Err(e) = r2_cleanup.delete_object(&object_key).await {
            tracing::warn!("R2 cleanup failed for {object_key}: {e}");
        }
    });

    tracing::info!(
        user_id = %user_id,
        page_count = page_count,
        credits_deducted = actual_credits,
        "Image PDF processed via R2 → Upstage pipeline"
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "success": true,
            "doc_type": "image_pdf",
            "engine": "upstage_document_parse",
            "page_count": page_count,
            "html": html,
            "markdown": markdown,
            "credits_deducted": actual_credits,
        })),
    )
}

