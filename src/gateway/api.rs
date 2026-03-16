//! REST API handlers for the web dashboard.
//!
//! All `/api/*` routes require bearer token authentication (PairingGuard).

use super::AppState;
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};
use chrono::Datelike;
use serde::Deserialize;

const MASKED_SECRET: &str = "***MASKED***";

// ── Bearer token auth extractor ─────────────────────────────────

/// Extract and validate bearer token from Authorization header.
fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
}

/// Verify bearer token against PairingGuard. Returns error response if unauthorized.
fn require_auth(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if !state.pairing.require_pairing() {
        return Ok(());
    }

    let token = extract_bearer_token(headers).unwrap_or("");
    if state.pairing.is_authenticated(token) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>"
            })),
        ))
    }
}

/// Resolve the Kakao ID for the current session user (for Supabase lookups).
///
/// Returns `None` if auth is not configured or the user has no Kakao link.
fn resolve_kakao_id_from_session(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let auth_store = state.auth_store.as_ref()?;
    let token = extract_bearer_token(headers)?;
    let session = auth_store.validate_session(token)?;
    auth_store
        .get_channel_uid_for_user("kakao", &session.user_id)
        .ok()
        .flatten()
}

// ── Query parameters ─────────────────────────────────────────────

#[derive(Deserialize)]
pub struct MemoryQuery {
    pub query: Option<String>,
    pub category: Option<String>,
}

#[derive(Deserialize)]
pub struct MemoryStoreBody {
    pub key: String,
    pub content: String,
    pub category: Option<String>,
}

#[derive(Deserialize)]
pub struct CronAddBody {
    pub name: Option<String>,
    pub schedule: String,
    pub command: String,
}

// ── Handlers ────────────────────────────────────────────────────

/// GET /api/status — system status overview
pub async fn handle_api_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    let health = crate::health::snapshot();

    let mut channels = serde_json::Map::new();

    for (channel, present) in config.channels_config.channels() {
        channels.insert(channel.name().to_string(), serde_json::Value::Bool(present));
    }

    let body = serde_json::json!({
        "provider": config.default_provider,
        "model": state.model,
        "temperature": state.temperature,
        "uptime_seconds": health.uptime_seconds,
        "gateway_port": config.gateway.port,
        "locale": "en",
        "memory_backend": state.mem.name(),
        "paired": state.pairing.is_paired(),
        "channels": channels,
        "health": health,
    });

    Json(body).into_response()
}

/// GET /api/config — current config (api_key masked)
pub async fn handle_api_config_get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();

    // Serialize to TOML after masking sensitive fields.
    let masked_config = mask_sensitive_fields(&config);
    let toml_str = match toml::to_string_pretty(&masked_config) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Failed to serialize config: {e}")})),
            )
                .into_response();
        }
    };

    Json(serde_json::json!({
        "format": "toml",
        "content": toml_str,
    }))
    .into_response()
}

/// PUT /api/config — update config from TOML body
pub async fn handle_api_config_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    // Parse the incoming TOML and normalize known dashboard-masked edge cases.
    let mut incoming_toml: toml::Value = match toml::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("Invalid TOML: {e}")})),
            )
                .into_response();
        }
    };
    normalize_dashboard_config_toml(&mut incoming_toml);
    let incoming: crate::config::Config = match incoming_toml.try_into() {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("Invalid TOML: {e}")})),
            )
                .into_response();
        }
    };

    let current_config = state.config.lock().clone();
    let new_config = hydrate_config_for_save(incoming, &current_config);

    if let Err(e) = new_config.validate() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("Invalid config: {e}")})),
        )
            .into_response();
    }

    // Save to disk
    if let Err(e) = new_config.save().await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to save config: {e}")})),
        )
            .into_response();
    }

    // Update in-memory config
    *state.config.lock() = new_config;

    Json(serde_json::json!({"status": "ok"})).into_response()
}

/// PUT /api/config/api-key — set a single provider API key.
///
/// Body: `{"provider": "openai", "api_key": "sk-..."}`
///
/// Updates the primary `api_key` in config for the given provider.
/// This lightweight endpoint avoids requiring the full config TOML roundtrip.
pub async fn handle_api_config_api_key_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let provider = body.get("provider").and_then(|v| v.as_str()).unwrap_or("");
    let api_key_field = body.get("api_key"); // None = field absent, Some("") = explicit clear
    let api_key = api_key_field.and_then(|v| v.as_str()).unwrap_or("");
    let has_api_key_field = api_key_field.is_some();
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|m| !m.is_empty());

    if provider.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "provider is required"})),
        )
            .into_response();
    }

    let mut config = state.config.lock().clone();

    // Map frontend provider names to backend provider names
    let backend_provider = match provider {
        "claude" => "anthropic",
        p => p,
    };

    // Store or remove the key in the per-provider map.
    // Only touch provider_api_keys when api_key field is explicitly present in the request.
    // This allows provider/model-only updates without accidentally wiping stored keys.
    if has_api_key_field {
        if api_key.is_empty() {
            config.provider_api_keys.remove(backend_provider);
        } else {
            config
                .provider_api_keys
                .insert(backend_provider.to_string(), api_key.to_string());
        }
    }

    // Also set the provider-specific env var so resolve_provider_credential()
    // picks it up for the current process lifetime.
    let env_var = match backend_provider {
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "gemini" | "google" | "google-gemini" => "GEMINI_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        "groq" => "GROQ_API_KEY",
        "mistral" => "MISTRAL_API_KEY",
        "xai" | "grok" => "XAI_API_KEY",
        "venice" => "VENICE_API_KEY",
        "ollama" => "OLLAMA_API_KEY",
        "cohere" => "COHERE_API_KEY",
        "perplexity" => "PERPLEXITY_API_KEY",
        "together" | "together-ai" => "TOGETHER_API_KEY",
        "fireworks" | "fireworks-ai" => "FIREWORKS_API_KEY",
        "hunyuan" | "tencent" => "HUNYUAN_API_KEY",
        "synthetic" => "SYNTHETIC_API_KEY",
        "ovhcloud" | "ovh" => "OVH_AI_ENDPOINTS_ACCESS_TOKEN",
        "astrai" => "ASTRAI_API_KEY",
        "sglang" => "SGLANG_API_KEY",
        "vllm" => "VLLM_API_KEY",
        "osaurus" => "OSAURUS_API_KEY",
        "telnyx" => "TELNYX_API_KEY",
        "nvidia" | "nvidia-nim" => "NVIDIA_API_KEY",
        "vercel" | "vercel-ai" => "VERCEL_API_KEY",
        "cloudflare" | "cloudflare-ai" => "CLOUDFLARE_API_KEY",
        _ => "",
    };
    if has_api_key_field && !env_var.is_empty() {
        std::env::set_var(env_var, api_key);
    }

    // Set config-level api_key to this provider's key and update default_provider.
    // Only update when setting a key, not when removing one.
    if has_api_key_field && !api_key.is_empty() {
        config.api_key = Some(api_key.to_string());
    }

    // Always update default_provider when explicitly provided (even without a key change).
    // This lets the frontend sync provider selection independently of key management.
    config.default_provider = Some(backend_provider.to_string());

    // Update default_model if provided.
    if let Some(model_name) = model {
        config.default_model = Some(model_name.to_string());
    }

    if let Err(e) = config.save().await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to save: {e}")})),
        )
            .into_response();
    }

    *state.config.lock() = config;

    Json(serde_json::json!({"status": "ok", "provider": backend_provider})).into_response()
}

/// GET /api/tools — list registered tool specs
pub async fn handle_api_tools(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let tools: Vec<serde_json::Value> = state
        .tools_registry
        .iter()
        .map(|spec| {
            serde_json::json!({
                "name": spec.name,
                "description": spec.description,
                "parameters": spec.parameters,
            })
        })
        .collect();

    Json(serde_json::json!({"tools": tools})).into_response()
}

/// GET /api/cron — list cron jobs
pub async fn handle_api_cron_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    match crate::cron::list_jobs(&config) {
        Ok(jobs) => {
            let jobs_json: Vec<serde_json::Value> = jobs
                .iter()
                .map(|job| {
                    serde_json::json!({
                        "id": job.id,
                        "name": job.name,
                        "command": job.command,
                        "next_run": job.next_run.to_rfc3339(),
                        "last_run": job.last_run.map(|t| t.to_rfc3339()),
                        "last_status": job.last_status,
                        "enabled": job.enabled,
                    })
                })
                .collect();
            Json(serde_json::json!({"jobs": jobs_json})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to list cron jobs: {e}")})),
        )
            .into_response(),
    }
}

/// POST /api/cron — add a new cron job
pub async fn handle_api_cron_add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CronAddBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    let schedule = crate::cron::Schedule::Cron {
        expr: body.schedule,
        tz: None,
    };

    match crate::cron::add_shell_job(&config, body.name, schedule, &body.command) {
        Ok(job) => Json(serde_json::json!({
            "status": "ok",
            "job": {
                "id": job.id,
                "name": job.name,
                "command": job.command,
                "enabled": job.enabled,
            }
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to add cron job: {e}")})),
        )
            .into_response(),
    }
}

/// DELETE /api/cron/:id — remove a cron job
pub async fn handle_api_cron_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    match crate::cron::remove_job(&config, &id) {
        Ok(()) => Json(serde_json::json!({"status": "ok"})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to remove cron job: {e}")})),
        )
            .into_response(),
    }
}

/// GET /api/integrations — list all integrations with status
pub async fn handle_api_integrations(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    let entries = crate::integrations::registry::all_integrations();

    let integrations: Vec<serde_json::Value> = entries
        .iter()
        .map(|entry| {
            let status = (entry.status_fn)(&config);
            serde_json::json!({
                "name": entry.name,
                "description": entry.description,
                "category": entry.category,
                "status": status,
            })
        })
        .collect();

    Json(serde_json::json!({"integrations": integrations})).into_response()
}

/// POST /api/doctor — run diagnostics
pub async fn handle_api_doctor(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    let results = crate::doctor::diagnose(&config);

    let ok_count = results
        .iter()
        .filter(|r| r.severity == crate::doctor::Severity::Ok)
        .count();
    let warn_count = results
        .iter()
        .filter(|r| r.severity == crate::doctor::Severity::Warn)
        .count();
    let error_count = results
        .iter()
        .filter(|r| r.severity == crate::doctor::Severity::Error)
        .count();

    Json(serde_json::json!({
        "results": results,
        "summary": {
            "ok": ok_count,
            "warnings": warn_count,
            "errors": error_count,
        }
    }))
    .into_response()
}

/// GET /api/memory — list or search memory entries
pub async fn handle_api_memory_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<MemoryQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    if let Some(ref query) = params.query {
        // Search mode
        match state.mem.recall(query, 50, None).await {
            Ok(entries) => Json(serde_json::json!({"entries": entries})).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Memory recall failed: {e}")})),
            )
                .into_response(),
        }
    } else {
        // List mode
        let category = params.category.as_deref().map(|cat| match cat {
            "core" => crate::memory::MemoryCategory::Core,
            "daily" => crate::memory::MemoryCategory::Daily,
            "conversation" => crate::memory::MemoryCategory::Conversation,
            other => crate::memory::MemoryCategory::Custom(other.to_string()),
        });

        match state.mem.list(category.as_ref(), None).await {
            Ok(entries) => Json(serde_json::json!({"entries": entries})).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Memory list failed: {e}")})),
            )
                .into_response(),
        }
    }
}

/// POST /api/memory — store a memory entry
pub async fn handle_api_memory_store(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<MemoryStoreBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let category = body
        .category
        .as_deref()
        .map(|cat| match cat {
            "core" => crate::memory::MemoryCategory::Core,
            "daily" => crate::memory::MemoryCategory::Daily,
            "conversation" => crate::memory::MemoryCategory::Conversation,
            other => crate::memory::MemoryCategory::Custom(other.to_string()),
        })
        .unwrap_or(crate::memory::MemoryCategory::Core);

    match state
        .mem
        .store(&body.key, &body.content, category, None)
        .await
    {
        Ok(()) => Json(serde_json::json!({"status": "ok"})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Memory store failed: {e}")})),
        )
            .into_response(),
    }
}

/// DELETE /api/memory/:key — delete a memory entry
pub async fn handle_api_memory_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    match state.mem.forget(&key).await {
        Ok(deleted) => {
            Json(serde_json::json!({"status": "ok", "deleted": deleted})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Memory forget failed: {e}")})),
        )
            .into_response(),
    }
}

/// GET /api/cost — cost summary
pub async fn handle_api_cost(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    if let Some(ref tracker) = state.cost_tracker {
        match tracker.get_summary() {
            Ok(summary) => Json(serde_json::json!({"cost": summary})).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Cost summary failed: {e}")})),
            )
                .into_response(),
        }
    } else {
        Json(serde_json::json!({
            "cost": {
                "session_cost_usd": 0.0,
                "daily_cost_usd": 0.0,
                "monthly_cost_usd": 0.0,
                "total_tokens": 0,
                "request_count": 0,
                "by_model": {},
            }
        }))
        .into_response()
    }
}

// ── Billing / Credits ────────────────────────────────────────────

/// GET /api/credits/balance — current credit balance for the user
pub async fn handle_api_credits_balance(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    // Try Supabase first (cloud source of truth for credits).
    if let Some(ref sb) = state.supabase {
        // Resolve the user's kakao_id from their session.
        let kakao_id = resolve_kakao_id_from_session(&state, &headers);
        if let Some(kakao_id) = kakao_id {
            match sb.get_or_create_user(&kakao_id).await {
                Ok(user) => {
                    return Json(serde_json::json!({
                        "balance": user.credits,
                        "total_spent": user.total_spent,
                        "enabled": true,
                        "source": "supabase",
                    }))
                    .into_response();
                }
                Err(e) => {
                    tracing::warn!("Supabase credit lookup failed, falling back to local: {e}");
                }
            }
        }
    }

    // Fallback to local PaymentManager.
    let Some(ref pm) = state.payment_manager else {
        return Json(serde_json::json!({"balance": 0, "enabled": false})).into_response();
    };

    let user_id = state
        .sync_coordinator
        .as_ref()
        .map(|sc| sc.device_id().to_string())
        .unwrap_or_else(|| "local_user".to_string());

    let pm_guard = pm.lock();
    match pm_guard.get_balance(&user_id) {
        Ok(balance) => {
            Json(serde_json::json!({"balance": balance, "enabled": true, "source": "local"}))
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to get balance: {e}")})),
        )
            .into_response(),
    }
}

/// GET /api/credits/packages — available credit packages
pub async fn handle_api_credits_packages(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let packages: Vec<serde_json::Value> = crate::billing::payment::CREDIT_PACKAGES
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "name": p.name,
                "price_krw": p.price_krw,
                "credits": p.credits,
            })
        })
        .collect();

    Json(serde_json::json!({"packages": packages})).into_response()
}

/// POST /api/credits/purchase — initiate credit purchase
pub async fn handle_api_credits_purchase(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let Some(ref pm) = state.payment_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Payment system not configured"})),
        )
            .into_response();
    };

    let package_id = body["package_id"].as_str().unwrap_or("").to_string();

    if package_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Missing package_id"})),
        )
            .into_response();
    }

    let user_id = state
        .sync_coordinator
        .as_ref()
        .map(|sc| sc.device_id().to_string())
        .unwrap_or_else(|| "local_user".to_string());

    let pm_guard = pm.lock();
    match pm_guard.initiate_payment(&user_id, &package_id) {
        Ok((record, kakao_req)) => Json(serde_json::json!({
            "status": "pending",
            "transaction_id": record.transaction_id,
            "package_id": record.package_id,
            "amount_krw": record.amount_krw,
            "credits": record.credits,
            "payment_url": kakao_req.approval_url,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Payment initiation failed: {e}")})),
        )
            .into_response(),
    }
}

/// GET /api/payment/approve — Kakao Pay approval callback
///
/// After Kakao Pay redirects the user here with `pg_token`, this endpoint
/// completes the payment and grants credits atomically.
pub async fn handle_api_payment_approve(
    State(state): State<AppState>,
    Query(params): Query<PaymentCallbackParams>,
) -> impl IntoResponse {
    let Some(ref pm) = state.payment_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Payment system not configured"})),
        )
            .into_response();
    };

    let tx_id = params.tx.unwrap_or_default();
    if tx_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Missing tx parameter"})),
        )
            .into_response();
    }

    let pm_guard = pm.lock();
    match pm_guard.complete_payment(&tx_id) {
        Ok(record) => {
            tracing::info!(
                transaction_id = %tx_id,
                credits = record.credits,
                "Payment completed — credits granted"
            );
            Json(serde_json::json!({
                "status": "completed",
                "transaction_id": record.transaction_id,
                "credits": record.credits,
                "message": "Credits added to your account",
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Payment completion failed: {e}")})),
        )
            .into_response(),
    }
}

/// GET /api/payment/cancel — Kakao Pay cancellation callback
pub async fn handle_api_payment_cancel(
    State(state): State<AppState>,
    Query(params): Query<PaymentCallbackParams>,
) -> impl IntoResponse {
    let Some(ref pm) = state.payment_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Payment system not configured"})),
        )
            .into_response();
    };

    let tx_id = params.tx.unwrap_or_default();
    if tx_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Missing tx parameter"})),
        )
            .into_response();
    }

    let pm_guard = pm.lock();
    match pm_guard.cancel_payment(&tx_id) {
        Ok(()) => {
            tracing::info!(transaction_id = %tx_id, "Payment cancelled by user");
            Json(serde_json::json!({
                "status": "cancelled",
                "transaction_id": tx_id,
                "message": "Payment cancelled",
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Cancellation failed: {e}")})),
        )
            .into_response(),
    }
}

/// GET /api/payment/fail — Kakao Pay failure callback
pub async fn handle_api_payment_fail(
    State(state): State<AppState>,
    Query(params): Query<PaymentCallbackParams>,
) -> impl IntoResponse {
    let Some(ref pm) = state.payment_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Payment system not configured"})),
        )
            .into_response();
    };

    let tx_id = params.tx.unwrap_or_default();
    if tx_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Missing tx parameter"})),
        )
            .into_response();
    }

    let pm_guard = pm.lock();
    match pm_guard.fail_payment(&tx_id) {
        Ok(()) => {
            tracing::warn!(transaction_id = %tx_id, "Payment failed");
            Json(serde_json::json!({
                "status": "failed",
                "transaction_id": tx_id,
                "message": "Payment failed",
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to record failure: {e}")})),
        )
            .into_response(),
    }
}

/// Query params for payment callback URLs.
#[derive(Debug, Deserialize)]
pub struct PaymentCallbackParams {
    /// Transaction ID.
    pub tx: Option<String>,
}

/// GET /api/credits/history — payment history for the current user
pub async fn handle_api_credits_history(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let Some(ref pm) = state.payment_manager else {
        return Json(serde_json::json!({"payments": [], "enabled": false})).into_response();
    };

    let user_id = state
        .sync_coordinator
        .as_ref()
        .map(|sc| sc.device_id().to_string())
        .unwrap_or_else(|| "local_user".to_string());

    let pm_guard = pm.lock();
    match pm_guard.list_user_payments(&user_id, 50) {
        Ok(payments) => {
            let records: Vec<serde_json::Value> = payments
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "transaction_id": p.transaction_id,
                        "package_id": p.package_id,
                        "amount_krw": p.amount_krw,
                        "credits": p.credits,
                        "status": p.status.as_str(),
                        "created_at": p.created_at,
                    })
                })
                .collect();
            Json(serde_json::json!({"payments": records, "enabled": true})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to get history: {e}")})),
        )
            .into_response(),
    }
}

/// GET /api/credits/usage — API usage cost summary
pub async fn handle_api_credits_usage(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let Some(ref ct) = state.cost_tracker else {
        return Json(serde_json::json!({"usage": null, "enabled": false})).into_response();
    };

    let today = chrono::Utc::now().date_naive();
    let today_cost = ct.get_daily_cost(today).unwrap_or(0.0);
    let month_cost = ct
        .get_monthly_cost(today.year(), today.month())
        .unwrap_or(0.0);

    let summary = ct.get_summary().ok();

    Json(serde_json::json!({
        "enabled": true,
        "today_usd": today_cost,
        "month_usd": month_cost,
        "summary": summary,
    }))
    .into_response()
}

// ── Checkout (Stripe + TossPayments) ─────────────────────────────

/// Query params for checkout callbacks.
#[derive(Debug, Deserialize)]
pub struct CheckoutCallbackParams {
    pub tx: Option<String>,
    pub provider: Option<String>,
    #[serde(rename = "paymentKey")]
    pub payment_key: Option<String>,
    #[serde(rename = "orderId")]
    pub order_id: Option<String>,
    pub amount: Option<u32>,
}

/// GET /api/credits/packages/usd — available USD credit packages (Stripe + Toss)
pub async fn handle_api_credits_packages_usd(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let packages: Vec<serde_json::Value> = crate::billing::checkout::USD_PACKAGES
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "name": p.name,
                "price_usd": format!("${}", p.price_cents / 100),
                "price_cents": p.price_cents,
                "price_krw": p.price_krw,
                "credits": p.credits,
            })
        })
        .collect();

    let providers = {
        let config = state.config.lock();
        let has_stripe = config.stripe_secret_key.is_some();
        let has_toss = config.toss_secret_key.is_some();
        serde_json::json!({
            "stripe": has_stripe,
            "toss": has_toss,
        })
    };

    Json(serde_json::json!({
        "packages": packages,
        "providers": providers,
    }))
    .into_response()
}

/// POST /api/checkout/create — create a checkout session (Stripe or Toss)
pub async fn handle_api_checkout_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<crate::billing::checkout::CheckoutRequest>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let package = match crate::billing::checkout::find_usd_package(&body.package_id) {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Invalid package_id"})),
            )
                .into_response();
        }
    };

    // Derive user_id from auth context; fall back to request body
    let user_id = if body.user_id.is_empty() {
        state
            .sync_coordinator
            .as_ref()
            .map(|sc| sc.device_id().to_string())
            .unwrap_or_else(|| "local_user".to_string())
    } else {
        body.user_id.clone()
    };

    let transaction_id = uuid::Uuid::new_v4().to_string();
    let callback_base_url = {
        let config = state.config.lock();
        config
            .callback_base_url
            .clone()
            .unwrap_or_else(|| "https://localhost:3541".to_string())
    };

    match body.provider {
        crate::billing::checkout::CheckoutProvider::Stripe => {
            let secret_key = {
                let config = state.config.lock();
                config.stripe_secret_key.clone()
            };
            let Some(key) = secret_key else {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({"error": "Stripe not configured"})),
                )
                    .into_response();
            };

            match crate::billing::checkout::create_stripe_session(
                &key,
                package,
                &transaction_id,
                &user_id,
                &callback_base_url,
                body.save_method,
            )
            .await
            {
                Ok(resp) => Json(serde_json::json!({
                    "checkout_url": resp.checkout_url,
                    "transaction_id": resp.transaction_id,
                    "provider": "stripe",
                }))
                .into_response(),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("Stripe session failed: {e}")})),
                )
                    .into_response(),
            }
        }
        crate::billing::checkout::CheckoutProvider::Toss => {
            let secret_key = {
                let config = state.config.lock();
                config.toss_secret_key.clone()
            };
            let Some(key) = secret_key else {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({"error": "TossPayments not configured"})),
                )
                    .into_response();
            };

            match crate::billing::checkout::create_toss_session(
                &key,
                package,
                &transaction_id,
                &user_id,
                &callback_base_url,
            )
            .await
            {
                Ok(resp) => Json(serde_json::json!({
                    "checkout_url": resp.checkout_url,
                    "transaction_id": resp.transaction_id,
                    "provider": "toss",
                }))
                .into_response(),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("Toss session failed: {e}")})),
                )
                    .into_response(),
            }
        }
    }
}

/// GET /api/checkout/success — checkout success callback (both Stripe and Toss)
pub async fn handle_api_checkout_success(
    State(state): State<AppState>,
    Query(params): Query<CheckoutCallbackParams>,
) -> impl IntoResponse {
    let tx_id = params.tx.unwrap_or_default();
    if tx_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Missing tx parameter"})),
        )
            .into_response();
    }

    let provider = params.provider.unwrap_or_default();

    // For TossPayments, confirm the payment first
    if provider == "toss" {
        let payment_key = params.payment_key.unwrap_or_default();
        let amount = params.amount.unwrap_or(0);

        if payment_key.is_empty() || amount == 0 {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Missing paymentKey or amount for Toss confirmation"})),
            )
                .into_response();
        }

        let secret_key = {
            let config = state.config.lock();
            config.toss_secret_key.clone()
        };

        if let Some(key) = secret_key {
            if let Err(e) =
                crate::billing::checkout::confirm_toss_payment(&key, &payment_key, &tx_id, amount)
                    .await
            {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("Toss confirmation failed: {e}")})),
                )
                    .into_response();
            }
        }
    }

    // Grant credits via PaymentManager
    let Some(ref pm) = state.payment_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Payment system not configured"})),
        )
            .into_response();
    };

    let pm_guard = pm.lock();
    match pm_guard.complete_payment(&tx_id) {
        Ok(record) => {
            tracing::info!(
                transaction_id = %tx_id,
                provider = %provider,
                credits = record.credits,
                "Checkout completed — credits granted"
            );
            Json(serde_json::json!({
                "status": "completed",
                "transaction_id": tx_id,
                "credits": record.credits,
                "provider": provider,
                "message": "Credits added to your account",
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Credit grant failed: {e}")})),
        )
            .into_response(),
    }
}

/// GET /api/checkout/cancel — checkout cancellation callback
pub async fn handle_api_checkout_cancel(
    State(_state): State<AppState>,
    Query(params): Query<CheckoutCallbackParams>,
) -> impl IntoResponse {
    let tx_id = params.tx.unwrap_or_default();
    tracing::info!(transaction_id = %tx_id, "Checkout cancelled by user");
    Json(serde_json::json!({
        "status": "cancelled",
        "transaction_id": tx_id,
        "message": "Payment cancelled",
    }))
    .into_response()
}

/// POST /api/checkout/webhook/stripe — Stripe webhook endpoint
pub async fn handle_api_checkout_webhook_stripe(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let sig_header = match headers.get("stripe-signature").and_then(|v| v.to_str().ok()) {
        Some(sig) => sig.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Missing Stripe-Signature header"})),
            )
                .into_response();
        }
    };

    let webhook_secret = {
        let config = state.config.lock();
        config.stripe_webhook_secret.clone()
    };

    let Some(secret) = webhook_secret else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Stripe webhook secret not configured"})),
        )
            .into_response();
    };

    let event = match crate::billing::checkout::verify_stripe_signature(&body, &sig_header, &secret)
    {
        Ok(ev) => ev,
        Err(e) => {
            tracing::warn!("Invalid Stripe webhook signature: {e}");
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Invalid signature"})),
            )
                .into_response();
        }
    };

    let event_type = event
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match event_type {
        "checkout.session.completed" | "payment_intent.succeeded" => {
            let tx_id = event
                .pointer("/data/object/metadata/transaction_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if !tx_id.is_empty() {
                if let Some(ref pm) = state.payment_manager {
                    let pm_guard = pm.lock();
                    if let Err(e) = pm_guard.complete_payment(tx_id) {
                        tracing::warn!(transaction_id = %tx_id, "Stripe webhook: credit grant failed: {e}");
                    } else {
                        tracing::info!(transaction_id = %tx_id, "Stripe webhook: credits granted");
                    }
                }
            }
        }
        _ => {
            tracing::debug!(event_type, "Stripe webhook: unhandled event type");
        }
    }

    Json(serde_json::json!({"received": true})).into_response()
}

// ── Admin: Model Pricing Registry ────────────────────────────────
//
// These endpoints allow operators to view and manage per-model API pricing.
// All pricing data is persisted in `model_pricing.toml` and used for
// credit billing calculations.

/// GET /api/admin/pricing — list all models with pricing, grouped by provider
pub async fn handle_api_admin_pricing_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let registry = state.pricing_registry.snapshot();
    let grouped = registry.by_provider();

    Json(serde_json::json!({
        "total_models": registry.models.len(),
        "providers": grouped,
        "credit_multiplier": state.config.lock().platform_routing.credit_multiplier,
        "vat_rate": state.config.lock().platform_routing.vat_rate,
    }))
    .into_response()
}

/// GET /api/admin/pricing/:model_id — get pricing for a single model
pub async fn handle_api_admin_pricing_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model_id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    match state.pricing_registry.get_model(&model_id) {
        Some(price) => Json(serde_json::json!({
            "model_id": model_id,
            "pricing": price,
        }))
        .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("Model '{}' not found in pricing registry", model_id)})),
        )
            .into_response(),
    }
}

/// Request body for upserting a model's pricing.
#[derive(Deserialize)]
pub struct UpsertPricingRequest {
    pub provider: String,
    pub display_name: String,
    pub input_per_million: f64,
    pub output_per_million: f64,
    #[serde(default)]
    pub note: Option<String>,
}

/// PUT /api/admin/pricing/:model_id — add or update a model's pricing
pub async fn handle_api_admin_pricing_upsert(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model_id): Path<String>,
    Json(body): Json<UpsertPricingRequest>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    // Validate input
    if body.input_per_million < 0.0 || body.output_per_million < 0.0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Prices must be non-negative"})),
        )
            .into_response();
    }

    if body.provider.trim().is_empty() || body.display_name.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "provider and display_name are required"})),
        )
            .into_response();
    }

    let price = crate::billing::ModelPrice {
        provider: body.provider,
        display_name: body.display_name,
        input_per_million: body.input_per_million,
        output_per_million: body.output_per_million,
        note: body.note,
    };

    match state
        .pricing_registry
        .upsert_and_save(model_id.clone(), price.clone())
    {
        Ok(()) => {
            tracing::info!(model_id = model_id.as_str(), "Model pricing updated");
            Json(serde_json::json!({
                "status": "ok",
                "model_id": model_id,
                "pricing": price,
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to save pricing: {e}")})),
        )
            .into_response(),
    }
}

/// DELETE /api/admin/pricing/:model_id — remove a model from the registry
pub async fn handle_api_admin_pricing_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model_id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    match state.pricing_registry.remove_and_save(&model_id) {
        Ok(Some(removed)) => Json(serde_json::json!({
            "status": "ok",
            "removed": {
                "model_id": model_id,
                "pricing": removed,
            }
        }))
        .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("Model '{}' not found", model_id)})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to save: {e}")})),
        )
            .into_response(),
    }
}

/// POST /api/admin/pricing/estimate — estimate credit cost for given usage
#[derive(Deserialize)]
pub struct EstimateCostRequest {
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

pub async fn handle_api_admin_pricing_estimate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<EstimateCostRequest>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let raw_cost = state
        .pricing_registry
        .estimate_cost(&body.model, body.input_tokens, body.output_tokens);
    let platform = &state.config.lock().platform_routing;
    let credit_charge = platform.credit_charge(raw_cost);
    let multiplier = platform.credit_multiplier;
    let vat = platform.vat_rate;

    Json(serde_json::json!({
        "model": body.model,
        "input_tokens": body.input_tokens,
        "output_tokens": body.output_tokens,
        "raw_api_cost_usd": raw_cost,
        "credit_multiplier": multiplier,
        "vat_rate": vat,
        "total_credit_charge_usd": credit_charge,
    }))
    .into_response()
}

/// GET /api/cli-tools — discovered CLI tools
pub async fn handle_api_cli_tools(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let tools = crate::tools::cli_discovery::discover_cli_tools(&[], &[]);

    Json(serde_json::json!({"cli_tools": tools})).into_response()
}

/// POST /api/document/process — Process an uploaded document.
///
/// Accepts multipart form upload with a document file.
/// Auto-detects document type and routes to the appropriate pipeline:
/// - Digital PDF → local text extraction + optional Gemini correction
/// - Image PDF → Upstage OCR + Gemini correction
/// - Office docs (HWP, DOCX, etc.) → Hancom DocsConverter
///
/// Returns JSON with `html` and `markdown` fields.
pub async fn handle_api_document_process(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: axum::extract::Multipart,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    // Extract file from multipart body
    let mut file_data: Option<Vec<u8>> = None;
    let mut original_filename = String::from("uploaded_document.bin");

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            // Extract original filename with extension for correct classification
            if let Some(fname) = field.file_name() {
                original_filename = fname.to_string();
            }
            match field.bytes().await {
                Ok(bytes) => file_data = Some(bytes.to_vec()),
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({"error": format!("Failed to read file field: {e}")})),
                    )
                        .into_response();
                }
            }
            break;
        }
    }

    let file_bytes = match file_data {
        Some(b) if !b.is_empty() => b,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "No file uploaded. Send a multipart field named 'file'."})),
            )
                .into_response();
        }
    };

    // Write to a unique temp file preserving the original extension
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let tmp_dir = std::env::temp_dir().join("moa_doc_upload");
    let _ = std::fs::create_dir_all(&tmp_dir);
    let tmp_path = tmp_dir.join(format!("doc_{timestamp}_{original_filename}"));

    if let Err(e) = std::fs::write(&tmp_path, &file_bytes) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to save file: {e}")})),
        )
            .into_response();
    }

    // Process using the document pipeline tool
    use crate::tools::traits::Tool;
    let security = crate::security::SecurityPolicy::default();
    let tool = crate::tools::document_pipeline::DocumentPipelineTool::new(security);
    let args = serde_json::json!({
        "file_path": tmp_path.to_string_lossy().as_ref(),
        "output_dir": tmp_dir.to_string_lossy().as_ref(),
    });

    match tool.execute(args).await {
        Ok(result) => {
            // Clean up temp file
            let _ = std::fs::remove_file(&tmp_path);

            if result.success {
                // The tool output is a JSON string with html, markdown, doc_type, etc.
                // Parse it and return the structured fields directly so the frontend
                // can access result.html, result.markdown without an extra wrapper.
                let parsed: serde_json::Value =
                    serde_json::from_str(&result.output).unwrap_or_else(|_| {
                        serde_json::json!({
                            "markdown": result.output,
                            "html": "",
                        })
                    });
                Json(parsed).into_response()
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": result.output})),
                )
                    .into_response()
            }
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Document processing failed: {e}")})),
            )
                .into_response()
        }
    }
}

/// GET /api/health — component health snapshot
pub async fn handle_api_health(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let snapshot = crate::health::snapshot();
    Json(serde_json::json!({"health": snapshot})).into_response()
}

/// GET /api/pairing/devices — list paired devices
pub async fn handle_api_pairing_devices(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let devices = state.pairing.paired_devices();
    Json(serde_json::json!({ "devices": devices })).into_response()
}

/// DELETE /api/pairing/devices/:id — revoke paired device
pub async fn handle_api_pairing_device_revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    if !state.pairing.revoke_device(&id) {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Paired device not found"})),
        )
            .into_response();
    }

    if let Err(e) = super::persist_pairing_tokens(state.config.clone(), &state.pairing).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to persist pairing state: {e}")})),
        )
            .into_response();
    }

    Json(serde_json::json!({"status": "ok", "revoked": true, "id": id})).into_response()
}

// ── Helpers ─────────────────────────────────────────────────────

fn normalize_dashboard_config_toml(root: &mut toml::Value) {
    // Dashboard editors may round-trip masked reliability api_keys as a single
    // string. Accept that shape by normalizing it back to a string array.
    let Some(root_table) = root.as_table_mut() else {
        return;
    };
    let Some(reliability) = root_table
        .get_mut("reliability")
        .and_then(toml::Value::as_table_mut)
    else {
        return;
    };
    let Some(api_keys) = reliability.get_mut("api_keys") else {
        return;
    };
    if let Some(single) = api_keys.as_str() {
        *api_keys = toml::Value::Array(vec![toml::Value::String(single.to_string())]);
    }
}

fn is_masked_secret(value: &str) -> bool {
    value == MASKED_SECRET
}

fn mask_optional_secret(value: &mut Option<String>) {
    if value.is_some() {
        *value = Some(MASKED_SECRET.to_string());
    }
}

fn mask_required_secret(value: &mut String) {
    if !value.is_empty() {
        *value = MASKED_SECRET.to_string();
    }
}

fn mask_vec_secrets(values: &mut [String]) {
    for value in values.iter_mut() {
        if !value.is_empty() {
            *value = MASKED_SECRET.to_string();
        }
    }
}

#[allow(clippy::ref_option)]
fn restore_optional_secret(value: &mut Option<String>, current: &Option<String>) {
    if value.as_deref().is_some_and(is_masked_secret) {
        *value = current.clone();
    }
}

fn restore_required_secret(value: &mut String, current: &str) {
    if is_masked_secret(value) {
        *value = current.to_string();
    }
}

fn restore_vec_secrets(values: &mut [String], current: &[String]) {
    for (idx, value) in values.iter_mut().enumerate() {
        if is_masked_secret(value) {
            if let Some(existing) = current.get(idx) {
                *value = existing.clone();
            }
        }
    }
}

fn mask_sensitive_fields(config: &crate::config::Config) -> crate::config::Config {
    let mut masked = config.clone();

    mask_optional_secret(&mut masked.api_key);
    for value in masked.provider_api_keys.values_mut() {
        if !value.is_empty() {
            *value = MASKED_SECRET.to_string();
        }
    }
    mask_vec_secrets(&mut masked.reliability.api_keys);
    mask_optional_secret(&mut masked.composio.api_key);
    mask_optional_secret(&mut masked.proxy.http_proxy);
    mask_optional_secret(&mut masked.proxy.https_proxy);
    mask_optional_secret(&mut masked.proxy.all_proxy);
    mask_optional_secret(&mut masked.transcription.api_key);
    mask_optional_secret(&mut masked.browser.computer_use.api_key);
    mask_optional_secret(&mut masked.web_fetch.api_key);
    mask_optional_secret(&mut masked.web_search.api_key);
    mask_optional_secret(&mut masked.web_search.brave_api_key);
    mask_optional_secret(&mut masked.web_search.perplexity_api_key);
    mask_optional_secret(&mut masked.web_search.exa_api_key);
    mask_optional_secret(&mut masked.web_search.jina_api_key);
    mask_optional_secret(&mut masked.storage.provider.config.db_url);
    if let Some(cloudflare) = masked.tunnel.cloudflare.as_mut() {
        mask_required_secret(&mut cloudflare.token);
    }
    if let Some(ngrok) = masked.tunnel.ngrok.as_mut() {
        mask_required_secret(&mut ngrok.auth_token);
    }

    for agent in masked.agents.values_mut() {
        mask_optional_secret(&mut agent.api_key);
    }

    if let Some(telegram) = masked.channels_config.telegram.as_mut() {
        mask_required_secret(&mut telegram.bot_token);
    }
    if let Some(discord) = masked.channels_config.discord.as_mut() {
        mask_required_secret(&mut discord.bot_token);
    }
    if let Some(slack) = masked.channels_config.slack.as_mut() {
        mask_required_secret(&mut slack.bot_token);
        mask_optional_secret(&mut slack.app_token);
    }
    if let Some(mattermost) = masked.channels_config.mattermost.as_mut() {
        mask_required_secret(&mut mattermost.bot_token);
    }
    if let Some(webhook) = masked.channels_config.webhook.as_mut() {
        mask_optional_secret(&mut webhook.secret);
    }
    if let Some(matrix) = masked.channels_config.matrix.as_mut() {
        mask_required_secret(&mut matrix.access_token);
    }
    if let Some(whatsapp) = masked.channels_config.whatsapp.as_mut() {
        mask_optional_secret(&mut whatsapp.access_token);
        mask_optional_secret(&mut whatsapp.app_secret);
        mask_optional_secret(&mut whatsapp.verify_token);
    }
    if let Some(linq) = masked.channels_config.linq.as_mut() {
        mask_required_secret(&mut linq.api_token);
        mask_optional_secret(&mut linq.signing_secret);
    }
    if let Some(github) = masked.channels_config.github.as_mut() {
        mask_required_secret(&mut github.access_token);
        mask_optional_secret(&mut github.webhook_secret);
    }
    if let Some(wati) = masked.channels_config.wati.as_mut() {
        mask_required_secret(&mut wati.api_token);
    }
    if let Some(nextcloud) = masked.channels_config.nextcloud_talk.as_mut() {
        mask_required_secret(&mut nextcloud.app_token);
        mask_optional_secret(&mut nextcloud.webhook_secret);
    }
    if let Some(email) = masked.channels_config.email.as_mut() {
        mask_required_secret(&mut email.password);
    }
    if let Some(irc) = masked.channels_config.irc.as_mut() {
        mask_optional_secret(&mut irc.server_password);
        mask_optional_secret(&mut irc.nickserv_password);
        mask_optional_secret(&mut irc.sasl_password);
    }
    if let Some(lark) = masked.channels_config.lark.as_mut() {
        mask_required_secret(&mut lark.app_secret);
        mask_optional_secret(&mut lark.encrypt_key);
        mask_optional_secret(&mut lark.verification_token);
    }
    if let Some(feishu) = masked.channels_config.feishu.as_mut() {
        mask_required_secret(&mut feishu.app_secret);
        mask_optional_secret(&mut feishu.encrypt_key);
        mask_optional_secret(&mut feishu.verification_token);
    }
    if let Some(dingtalk) = masked.channels_config.dingtalk.as_mut() {
        mask_required_secret(&mut dingtalk.client_secret);
    }
    if let Some(napcat) = masked.channels_config.napcat.as_mut() {
        mask_optional_secret(&mut napcat.access_token);
    }
    if let Some(qq) = masked.channels_config.qq.as_mut() {
        mask_required_secret(&mut qq.app_secret);
    }
    if let Some(nostr) = masked.channels_config.nostr.as_mut() {
        mask_required_secret(&mut nostr.private_key);
    }
    if let Some(clawdtalk) = masked.channels_config.clawdtalk.as_mut() {
        mask_required_secret(&mut clawdtalk.api_key);
        mask_optional_secret(&mut clawdtalk.webhook_secret);
    }
    masked
}

fn restore_masked_sensitive_fields(
    incoming: &mut crate::config::Config,
    current: &crate::config::Config,
) {
    restore_optional_secret(&mut incoming.api_key, &current.api_key);
    for (key, value) in &mut incoming.provider_api_keys {
        if value == MASKED_SECRET {
            if let Some(original) = current.provider_api_keys.get(key) {
                *value = original.clone();
            }
        }
    }
    // Preserve provider_api_keys entries that exist in current but were removed from incoming
    // (the frontend may not include empty/masked entries)
    for (key, value) in &current.provider_api_keys {
        if !value.is_empty() {
            incoming
                .provider_api_keys
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
    }
    restore_vec_secrets(
        &mut incoming.reliability.api_keys,
        &current.reliability.api_keys,
    );
    restore_optional_secret(&mut incoming.composio.api_key, &current.composio.api_key);
    restore_optional_secret(&mut incoming.proxy.http_proxy, &current.proxy.http_proxy);
    restore_optional_secret(&mut incoming.proxy.https_proxy, &current.proxy.https_proxy);
    restore_optional_secret(&mut incoming.proxy.all_proxy, &current.proxy.all_proxy);
    restore_optional_secret(
        &mut incoming.transcription.api_key,
        &current.transcription.api_key,
    );
    restore_optional_secret(
        &mut incoming.browser.computer_use.api_key,
        &current.browser.computer_use.api_key,
    );
    restore_optional_secret(&mut incoming.web_fetch.api_key, &current.web_fetch.api_key);
    restore_optional_secret(
        &mut incoming.web_search.api_key,
        &current.web_search.api_key,
    );
    restore_optional_secret(
        &mut incoming.web_search.brave_api_key,
        &current.web_search.brave_api_key,
    );
    restore_optional_secret(
        &mut incoming.web_search.perplexity_api_key,
        &current.web_search.perplexity_api_key,
    );
    restore_optional_secret(
        &mut incoming.web_search.exa_api_key,
        &current.web_search.exa_api_key,
    );
    restore_optional_secret(
        &mut incoming.web_search.jina_api_key,
        &current.web_search.jina_api_key,
    );
    restore_optional_secret(
        &mut incoming.storage.provider.config.db_url,
        &current.storage.provider.config.db_url,
    );
    if let (Some(incoming_tunnel), Some(current_tunnel)) = (
        incoming.tunnel.cloudflare.as_mut(),
        current.tunnel.cloudflare.as_ref(),
    ) {
        restore_required_secret(&mut incoming_tunnel.token, &current_tunnel.token);
    }
    if let (Some(incoming_tunnel), Some(current_tunnel)) = (
        incoming.tunnel.ngrok.as_mut(),
        current.tunnel.ngrok.as_ref(),
    ) {
        restore_required_secret(&mut incoming_tunnel.auth_token, &current_tunnel.auth_token);
    }

    for (name, agent) in &mut incoming.agents {
        if let Some(current_agent) = current.agents.get(name) {
            restore_optional_secret(&mut agent.api_key, &current_agent.api_key);
        }
    }

    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.telegram.as_mut(),
        current.channels_config.telegram.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.bot_token, &current_ch.bot_token);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.discord.as_mut(),
        current.channels_config.discord.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.bot_token, &current_ch.bot_token);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.slack.as_mut(),
        current.channels_config.slack.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.bot_token, &current_ch.bot_token);
        restore_optional_secret(&mut incoming_ch.app_token, &current_ch.app_token);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.mattermost.as_mut(),
        current.channels_config.mattermost.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.bot_token, &current_ch.bot_token);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.webhook.as_mut(),
        current.channels_config.webhook.as_ref(),
    ) {
        restore_optional_secret(&mut incoming_ch.secret, &current_ch.secret);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.matrix.as_mut(),
        current.channels_config.matrix.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.access_token, &current_ch.access_token);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.whatsapp.as_mut(),
        current.channels_config.whatsapp.as_ref(),
    ) {
        restore_optional_secret(&mut incoming_ch.access_token, &current_ch.access_token);
        restore_optional_secret(&mut incoming_ch.app_secret, &current_ch.app_secret);
        restore_optional_secret(&mut incoming_ch.verify_token, &current_ch.verify_token);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.linq.as_mut(),
        current.channels_config.linq.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.api_token, &current_ch.api_token);
        restore_optional_secret(&mut incoming_ch.signing_secret, &current_ch.signing_secret);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.github.as_mut(),
        current.channels_config.github.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.access_token, &current_ch.access_token);
        restore_optional_secret(&mut incoming_ch.webhook_secret, &current_ch.webhook_secret);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.wati.as_mut(),
        current.channels_config.wati.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.api_token, &current_ch.api_token);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.nextcloud_talk.as_mut(),
        current.channels_config.nextcloud_talk.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.app_token, &current_ch.app_token);
        restore_optional_secret(&mut incoming_ch.webhook_secret, &current_ch.webhook_secret);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.email.as_mut(),
        current.channels_config.email.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.password, &current_ch.password);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.irc.as_mut(),
        current.channels_config.irc.as_ref(),
    ) {
        restore_optional_secret(
            &mut incoming_ch.server_password,
            &current_ch.server_password,
        );
        restore_optional_secret(
            &mut incoming_ch.nickserv_password,
            &current_ch.nickserv_password,
        );
        restore_optional_secret(&mut incoming_ch.sasl_password, &current_ch.sasl_password);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.lark.as_mut(),
        current.channels_config.lark.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.app_secret, &current_ch.app_secret);
        restore_optional_secret(&mut incoming_ch.encrypt_key, &current_ch.encrypt_key);
        restore_optional_secret(
            &mut incoming_ch.verification_token,
            &current_ch.verification_token,
        );
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.feishu.as_mut(),
        current.channels_config.feishu.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.app_secret, &current_ch.app_secret);
        restore_optional_secret(&mut incoming_ch.encrypt_key, &current_ch.encrypt_key);
        restore_optional_secret(
            &mut incoming_ch.verification_token,
            &current_ch.verification_token,
        );
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.dingtalk.as_mut(),
        current.channels_config.dingtalk.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.client_secret, &current_ch.client_secret);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.napcat.as_mut(),
        current.channels_config.napcat.as_ref(),
    ) {
        restore_optional_secret(&mut incoming_ch.access_token, &current_ch.access_token);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.qq.as_mut(),
        current.channels_config.qq.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.app_secret, &current_ch.app_secret);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.nostr.as_mut(),
        current.channels_config.nostr.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.private_key, &current_ch.private_key);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.clawdtalk.as_mut(),
        current.channels_config.clawdtalk.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.api_key, &current_ch.api_key);
        restore_optional_secret(&mut incoming_ch.webhook_secret, &current_ch.webhook_secret);
    }
}

fn hydrate_config_for_save(
    mut incoming: crate::config::Config,
    current: &crate::config::Config,
) -> crate::config::Config {
    restore_masked_sensitive_fields(&mut incoming, current);
    // These are runtime-computed fields skipped from TOML serialization.
    incoming.config_path = current.config_path.clone();
    incoming.workspace_dir = current.workspace_dir.clone();
    incoming
}

// ── Sync endpoints (cross-device memory sync) ────────────────────

/// Push local deltas to the sync coordinator for relay to other devices.
///
/// Request body:
/// ```json
/// { "version_vector": { "clocks": { "device_a": 5 } } }
/// ```
///
/// Response: encrypted sync payload with deltas the requester hasn't seen.
pub async fn handle_sync_push(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let coordinator = match &state.sync_coordinator {
        Some(c) => c.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "Sync not enabled" })),
            )
                .into_response();
        }
    };

    // Parse the incoming message and delegate to coordinator
    let json_str = body.to_string();
    let responses = coordinator.handle_message(&json_str).await;

    Json(serde_json::json!({
        "status": "ok",
        "device_id": coordinator.device_id(),
        "responses": responses,
    }))
    .into_response()
}

/// Pull missing deltas from this device's sync coordinator.
///
/// Request body:
/// ```json
/// { "version_vector": { "clocks": { "device_b": 3 } } }
/// ```
///
/// Response: deltas the requester hasn't seen.
pub async fn handle_sync_pull(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let coordinator = match &state.sync_coordinator {
        Some(c) => c.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "Sync not enabled" })),
            )
                .into_response();
        }
    };

    // Build a SyncRequest message from the body and process it
    let from_device = body
        .get("device_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let version_vector = body.get("version_vector").cloned().unwrap_or_default();

    let sync_request = serde_json::json!({
        "SyncRequest": {
            "from_device_id": from_device,
            "version_vector": version_vector,
        }
    });

    let responses = coordinator.handle_message(&sync_request.to_string()).await;

    Json(serde_json::json!({
        "status": "ok",
        "device_id": coordinator.device_id(),
        "version": coordinator.version(),
        "responses": responses,
    }))
    .into_response()
}

/// Get current sync status (device ID, version vector, journal size).
pub async fn handle_sync_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    match &state.sync_coordinator {
        Some(coordinator) => Json(serde_json::json!({
            "enabled": true,
            "device_id": coordinator.device_id(),
            "version": coordinator.version(),
        }))
        .into_response(),
        None => Json(serde_json::json!({
            "enabled": false,
            "device_id": null,
            "version": null,
        }))
        .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{
        CloudflareTunnelConfig, LarkReceiveMode, NgrokTunnelConfig, WatiConfig,
    };

    #[test]
    fn masking_keeps_toml_valid_and_preserves_api_keys_type() {
        let mut cfg = crate::config::Config::default();
        cfg.api_key = Some("sk-live-123".to_string());
        cfg.reliability.api_keys = vec!["rk-1".to_string(), "rk-2".to_string()];

        let masked = mask_sensitive_fields(&cfg);
        let toml = toml::to_string_pretty(&masked).expect("masked config should serialize");
        let parsed: crate::config::Config =
            toml::from_str(&toml).expect("masked config should remain valid TOML for Config");

        assert_eq!(parsed.api_key.as_deref(), Some(MASKED_SECRET));
        assert_eq!(
            parsed.reliability.api_keys,
            vec![MASKED_SECRET.to_string(), MASKED_SECRET.to_string()]
        );
    }

    #[test]
    fn hydrate_config_for_save_restores_masked_secrets_and_paths() {
        let mut current = crate::config::Config::default();
        current.config_path = std::path::PathBuf::from("/tmp/current/config.toml");
        current.workspace_dir = std::path::PathBuf::from("/tmp/current/workspace");
        current.api_key = Some("real-key".to_string());
        current.transcription.api_key = Some("transcription-real-key".to_string());
        current.reliability.api_keys = vec!["r1".to_string(), "r2".to_string()];

        let mut incoming = mask_sensitive_fields(&current);
        incoming.default_model = Some("gpt-4.1-mini".to_string());
        // Simulate UI changing only one key and keeping the first masked.
        incoming.reliability.api_keys = vec![MASKED_SECRET.to_string(), "r2-new".to_string()];

        let hydrated = hydrate_config_for_save(incoming, &current);

        assert_eq!(hydrated.config_path, current.config_path);
        assert_eq!(hydrated.workspace_dir, current.workspace_dir);
        assert_eq!(hydrated.api_key, current.api_key);
        assert_eq!(
            hydrated.transcription.api_key,
            current.transcription.api_key
        );
        assert_eq!(hydrated.default_model.as_deref(), Some("gpt-4.1-mini"));
        assert_eq!(
            hydrated.reliability.api_keys,
            vec!["r1".to_string(), "r2-new".to_string()]
        );
    }

    #[test]
    fn normalize_dashboard_config_toml_promotes_single_api_key_string_to_array() {
        let mut cfg = crate::config::Config::default();
        cfg.reliability.api_keys = vec!["rk-live".to_string()];
        let raw_toml = toml::to_string_pretty(&cfg).expect("config should serialize");
        let mut raw =
            toml::from_str::<toml::Value>(&raw_toml).expect("serialized config should parse");
        raw.as_table_mut()
            .and_then(|root| root.get_mut("reliability"))
            .and_then(toml::Value::as_table_mut)
            .and_then(|reliability| reliability.get_mut("api_keys"))
            .map(|api_keys| *api_keys = toml::Value::String(MASKED_SECRET.to_string()))
            .expect("reliability.api_keys should exist");

        normalize_dashboard_config_toml(&mut raw);

        let parsed: crate::config::Config = raw
            .try_into()
            .expect("normalized toml should parse as Config");
        assert_eq!(parsed.reliability.api_keys, vec![MASKED_SECRET.to_string()]);
    }

    #[test]
    fn mask_sensitive_fields_covers_wati_email_and_feishu_secrets() {
        let mut cfg = crate::config::Config::default();
        cfg.proxy.http_proxy = Some("http://user:pass@proxy.internal:8080".to_string());
        cfg.proxy.https_proxy = Some("https://user:pass@proxy.internal:8443".to_string());
        cfg.proxy.all_proxy = Some("socks5://user:pass@proxy.internal:1080".to_string());
        cfg.transcription.api_key = Some("transcription-real-key".to_string());
        cfg.web_search.api_key = Some("web-search-generic-key".to_string());
        cfg.web_search.brave_api_key = Some("web-search-brave-key".to_string());
        cfg.web_search.perplexity_api_key = Some("web-search-perplexity-key".to_string());
        cfg.web_search.exa_api_key = Some("web-search-exa-key".to_string());
        cfg.web_search.jina_api_key = Some("web-search-jina-key".to_string());
        cfg.tunnel.cloudflare = Some(CloudflareTunnelConfig {
            token: "cloudflare-real-token".to_string(),
        });
        cfg.tunnel.ngrok = Some(NgrokTunnelConfig {
            auth_token: "ngrok-real-token".to_string(),
            domain: Some("zeroclaw.ngrok.app".to_string()),
        });
        cfg.channels_config.wati = Some(WatiConfig {
            api_token: "wati-real-token".to_string(),
            api_url: "https://live-mt-server.wati.io".to_string(),
            tenant_id: Some("tenant-1".to_string()),
            allowed_numbers: vec!["*".to_string()],
        });
        let mut email = crate::channels::email_channel::EmailConfig::default();
        email.password = "email-real-password".to_string();
        cfg.channels_config.email = Some(email);
        cfg.channels_config.feishu = Some(crate::config::FeishuConfig {
            app_id: "cli_app_id".to_string(),
            app_secret: "feishu-real-secret".to_string(),
            encrypt_key: Some("feishu-encrypt-key".to_string()),
            verification_token: Some("feishu-verify-token".to_string()),
            allowed_users: vec!["*".to_string()],
            group_reply: None,
            receive_mode: LarkReceiveMode::Webhook,
            port: Some(42617),
            draft_update_interval_ms: crate::config::schema::default_lark_draft_update_interval_ms(
            ),
            max_draft_edits: crate::config::schema::default_lark_max_draft_edits(),
        });

        let masked = mask_sensitive_fields(&cfg);
        assert_eq!(masked.proxy.http_proxy.as_deref(), Some(MASKED_SECRET));
        assert_eq!(masked.proxy.https_proxy.as_deref(), Some(MASKED_SECRET));
        assert_eq!(masked.proxy.all_proxy.as_deref(), Some(MASKED_SECRET));
        assert_eq!(masked.transcription.api_key.as_deref(), Some(MASKED_SECRET));
        assert_eq!(masked.web_search.api_key.as_deref(), Some(MASKED_SECRET));
        assert_eq!(
            masked.web_search.brave_api_key.as_deref(),
            Some(MASKED_SECRET)
        );
        assert_eq!(
            masked.web_search.perplexity_api_key.as_deref(),
            Some(MASKED_SECRET)
        );
        assert_eq!(
            masked.web_search.exa_api_key.as_deref(),
            Some(MASKED_SECRET)
        );
        assert_eq!(
            masked.web_search.jina_api_key.as_deref(),
            Some(MASKED_SECRET)
        );
        assert_eq!(
            masked
                .tunnel
                .cloudflare
                .as_ref()
                .map(|value| value.token.as_str()),
            Some(MASKED_SECRET)
        );
        assert_eq!(
            masked
                .tunnel
                .ngrok
                .as_ref()
                .map(|value| value.auth_token.as_str()),
            Some(MASKED_SECRET)
        );
        assert_eq!(
            masked
                .channels_config
                .wati
                .as_ref()
                .map(|value| value.api_token.as_str()),
            Some(MASKED_SECRET)
        );
        assert_eq!(
            masked
                .channels_config
                .email
                .as_ref()
                .map(|value| value.password.as_str()),
            Some(MASKED_SECRET)
        );
        let masked_feishu = masked
            .channels_config
            .feishu
            .as_ref()
            .expect("feishu config should exist");
        assert_eq!(masked_feishu.app_secret, MASKED_SECRET);
        assert_eq!(masked_feishu.encrypt_key.as_deref(), Some(MASKED_SECRET));
        assert_eq!(
            masked_feishu.verification_token.as_deref(),
            Some(MASKED_SECRET)
        );
    }

    #[test]
    fn hydrate_config_for_save_restores_wati_email_and_feishu_secrets() {
        let mut current = crate::config::Config::default();
        current.proxy.http_proxy = Some("http://user:pass@proxy.internal:8080".to_string());
        current.proxy.https_proxy = Some("https://user:pass@proxy.internal:8443".to_string());
        current.proxy.all_proxy = Some("socks5://user:pass@proxy.internal:1080".to_string());
        current.web_search.api_key = Some("web-search-generic-key".to_string());
        current.web_search.brave_api_key = Some("web-search-brave-key".to_string());
        current.web_search.perplexity_api_key = Some("web-search-perplexity-key".to_string());
        current.web_search.exa_api_key = Some("web-search-exa-key".to_string());
        current.web_search.jina_api_key = Some("web-search-jina-key".to_string());
        current.tunnel.cloudflare = Some(CloudflareTunnelConfig {
            token: "cloudflare-real-token".to_string(),
        });
        current.tunnel.ngrok = Some(NgrokTunnelConfig {
            auth_token: "ngrok-real-token".to_string(),
            domain: Some("zeroclaw.ngrok.app".to_string()),
        });
        current.channels_config.wati = Some(WatiConfig {
            api_token: "wati-real-token".to_string(),
            api_url: "https://live-mt-server.wati.io".to_string(),
            tenant_id: Some("tenant-1".to_string()),
            allowed_numbers: vec!["*".to_string()],
        });
        let mut email = crate::channels::email_channel::EmailConfig::default();
        email.password = "email-real-password".to_string();
        current.channels_config.email = Some(email);
        current.channels_config.feishu = Some(crate::config::FeishuConfig {
            app_id: "cli_app_id".to_string(),
            app_secret: "feishu-real-secret".to_string(),
            encrypt_key: Some("feishu-encrypt-key".to_string()),
            verification_token: Some("feishu-verify-token".to_string()),
            allowed_users: vec!["*".to_string()],
            group_reply: None,
            receive_mode: LarkReceiveMode::Webhook,
            port: Some(42617),
            draft_update_interval_ms: crate::config::schema::default_lark_draft_update_interval_ms(
            ),
            max_draft_edits: crate::config::schema::default_lark_max_draft_edits(),
        });

        let incoming = mask_sensitive_fields(&current);
        let restored = hydrate_config_for_save(incoming, &current);

        assert_eq!(
            restored.proxy.http_proxy.as_deref(),
            Some("http://user:pass@proxy.internal:8080")
        );
        assert_eq!(
            restored.proxy.https_proxy.as_deref(),
            Some("https://user:pass@proxy.internal:8443")
        );
        assert_eq!(
            restored.proxy.all_proxy.as_deref(),
            Some("socks5://user:pass@proxy.internal:1080")
        );
        assert_eq!(
            restored.web_search.api_key.as_deref(),
            Some("web-search-generic-key")
        );
        assert_eq!(
            restored.web_search.brave_api_key.as_deref(),
            Some("web-search-brave-key")
        );
        assert_eq!(
            restored.web_search.perplexity_api_key.as_deref(),
            Some("web-search-perplexity-key")
        );
        assert_eq!(
            restored.web_search.exa_api_key.as_deref(),
            Some("web-search-exa-key")
        );
        assert_eq!(
            restored.web_search.jina_api_key.as_deref(),
            Some("web-search-jina-key")
        );
        assert_eq!(
            restored
                .tunnel
                .cloudflare
                .as_ref()
                .map(|value| value.token.as_str()),
            Some("cloudflare-real-token")
        );
        assert_eq!(
            restored
                .tunnel
                .ngrok
                .as_ref()
                .map(|value| value.auth_token.as_str()),
            Some("ngrok-real-token")
        );
        assert_eq!(
            restored
                .channels_config
                .wati
                .as_ref()
                .map(|value| value.api_token.as_str()),
            Some("wati-real-token")
        );
        assert_eq!(
            restored
                .channels_config
                .email
                .as_ref()
                .map(|value| value.password.as_str()),
            Some("email-real-password")
        );
        let restored_feishu = restored
            .channels_config
            .feishu
            .as_ref()
            .expect("feishu config should exist");
        assert_eq!(restored_feishu.app_secret, "feishu-real-secret");
        assert_eq!(
            restored_feishu.encrypt_key.as_deref(),
            Some("feishu-encrypt-key")
        );
        assert_eq!(
            restored_feishu.verification_token.as_deref(),
            Some("feishu-verify-token")
        );
    }
}
