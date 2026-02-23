//! REST API handlers for the web dashboard.
//!
//! All `/api/*` routes require bearer token authentication (PairingGuard).

use super::AppState;
use crate::providers::{
    is_glm_alias, is_minimax_alias, is_moonshot_alias, is_qianfan_alias, is_qwen_alias,
    is_zai_alias, provider_credential_available,
};
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
};

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

#[derive(Deserialize)]
pub struct IntegrationCredentialsUpdateBody {
    pub revision: Option<String>,
    #[serde(default)]
    pub fields: HashMap<String, String>,
}

#[derive(Serialize)]
struct IntegrationCredentialsField {
    key: &'static str,
    label: &'static str,
    required: bool,
    has_value: bool,
    input_type: &'static str,
    options: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    masked_value: Option<&'static str>,
}

#[derive(Serialize)]
struct IntegrationSettingsEntry {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    category: crate::integrations::IntegrationCategory,
    status: crate::integrations::IntegrationStatus,
    configured: bool,
    activates_default_provider: bool,
    fields: Vec<IntegrationCredentialsField>,
}

#[derive(Serialize)]
struct IntegrationSettingsPayload {
    revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_default_provider_integration_id: Option<&'static str>,
    integrations: Vec<IntegrationSettingsEntry>,
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
    let model = config
        .default_model
        .clone()
        .unwrap_or_else(|| state.model.clone());

    let mut channels = serde_json::Map::new();

    for (channel, present) in config.channels_config.channels() {
        channels.insert(channel.name().to_string(), serde_json::Value::Bool(present));
    }

    let body = serde_json::json!({
        "provider": config.default_provider,
        "model": model,
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

    // Serialize to TOML, then mask sensitive fields
    let toml_str = match toml::to_string_pretty(&config) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Failed to serialize config: {e}")})),
            )
                .into_response();
        }
    };

    // Mask api_key in the TOML output
    let masked = mask_sensitive_fields(&toml_str);

    Json(serde_json::json!({
        "format": "toml",
        "content": masked,
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

    // Parse the incoming TOML
    let new_config: crate::config::Config = match toml::from_str(&body) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("Invalid TOML: {e}")})),
            )
                .into_response();
        }
    };

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

/// GET /api/integrations/settings — configurable credential fields for integrations
pub async fn handle_api_integrations_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    let entries = crate::integrations::registry::all_integrations();
    let revision = match config_revision(&config) {
        Ok(value) => value,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": err})),
            )
                .into_response();
        }
    };

    let integrations = entries
        .iter()
        .filter_map(|entry| {
            let id = integration_id_from_name(entry.name)?;
            let (configured, fields) = integration_fields(id, &config);
            Some(IntegrationSettingsEntry {
                id,
                name: entry.name,
                description: entry.description,
                category: entry.category,
                status: (entry.status_fn)(&config),
                configured,
                activates_default_provider: is_ai_integration_id(id),
                fields,
            })
        })
        .collect();

    let active_default_provider_integration_id =
        active_ai_integration_id(config.default_provider.as_deref());

    Json(IntegrationSettingsPayload {
        revision,
        active_default_provider_integration_id,
        integrations,
    })
    .into_response()
}

/// PUT /api/integrations/:id/credentials — update credential fields for one integration
pub async fn handle_api_integration_credentials_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(integration_id): Path<String>,
    Json(body): Json<IntegrationCredentialsUpdateBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let current_config = state.config.lock().clone();
    let current_revision = match config_revision(&current_config) {
        Ok(value) => value,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": err})),
            )
                .into_response();
        }
    };

    let mut updated_config = current_config.clone();
    if let Err(err) =
        apply_integration_credentials(&mut updated_config, &integration_id, &body.fields)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": err})),
        )
            .into_response();
    }

    let updated_revision = match config_revision(&updated_config) {
        Ok(value) => value,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": err})),
            )
                .into_response();
        }
    };

    if let Some(expected_revision) = body
        .revision
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if expected_revision != current_revision {
            if updated_revision == current_revision {
                return Json(
                    serde_json::json!({"status": "ok", "revision": current_revision, "unchanged": true}),
                )
                .into_response();
            }

            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "Configuration changed. Refresh integration settings and retry.",
                    "revision": current_revision,
                })),
            )
                .into_response();
        }
    }

    if updated_revision == current_revision {
        return Json(
            serde_json::json!({"status": "ok", "revision": current_revision, "unchanged": true}),
        )
        .into_response();
    }

    if let Err(err) = updated_config.save().await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to save config: {err}")})),
        )
            .into_response();
    }

    *state.config.lock() = updated_config;

    Json(serde_json::json!({
        "status": "ok",
        "revision": updated_revision,
        "unchanged": false,
    }))
    .into_response()
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

// ── Helpers ─────────────────────────────────────────────────────

fn config_revision(config: &crate::config::Config) -> Result<String, String> {
    let toml_str = toml::to_string(config)
        .map_err(|err| format!("Failed to serialize config for revision: {err}"))?;
    let mut hasher = DefaultHasher::new();
    toml_str.hash(&mut hasher);
    Ok(format!("{:016x}", hasher.finish()))
}

fn integration_id_from_name(name: &str) -> Option<&'static str> {
    match name {
        "Telegram" => Some("telegram"),
        "Discord" => Some("discord"),
        "Slack" => Some("slack"),
        "OpenRouter" => Some("openrouter"),
        "Anthropic" => Some("anthropic"),
        "OpenAI" => Some("openai"),
        "Google" => Some("google"),
        "DeepSeek" => Some("deepseek"),
        "xAI" => Some("xai"),
        "Mistral" => Some("mistral"),
        "Ollama" => Some("ollama"),
        "Perplexity" => Some("perplexity"),
        "Venice" => Some("venice"),
        "Vercel AI" => Some("vercel"),
        "Cloudflare AI" => Some("cloudflare"),
        "Moonshot" => Some("moonshot"),
        "Synthetic" => Some("synthetic"),
        "OpenCode Zen" => Some("opencode"),
        "Z.AI" => Some("zai"),
        "GLM" => Some("glm"),
        "MiniMax" => Some("minimax"),
        "Qwen" => Some("qwen"),
        "Amazon Bedrock" => Some("bedrock"),
        "Qianfan" => Some("qianfan"),
        "Groq" => Some("groq"),
        "Together AI" => Some("together"),
        "Fireworks AI" => Some("fireworks"),
        "Cohere" => Some("cohere"),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct AiIntegrationSpec {
    provider: &'static str,
    requires_api_key: bool,
    status_model_prefix: Option<&'static str>,
    default_model: Option<&'static str>,
    top_models: &'static [&'static str],
    supports_api_url: bool,
}

fn ai_integration_spec(integration_id: &str) -> Option<AiIntegrationSpec> {
    match integration_id {
        "openrouter" => Some(AiIntegrationSpec {
            provider: "openrouter",
            requires_api_key: true,
            status_model_prefix: None,
            default_model: Some("anthropic/claude-sonnet-4-6"),
            top_models: &[
                "anthropic/claude-sonnet-4-6",
                "openai/gpt-5.2",
                "google/gemini-3.1-pro",
            ],
            supports_api_url: false,
        }),
        "anthropic" => Some(AiIntegrationSpec {
            provider: "anthropic",
            requires_api_key: true,
            status_model_prefix: None,
            default_model: Some("claude-sonnet-4-6"),
            top_models: &["claude-sonnet-4-6", "claude-opus-4-6"],
            supports_api_url: false,
        }),
        "openai" => Some(AiIntegrationSpec {
            provider: "openai",
            requires_api_key: true,
            status_model_prefix: None,
            default_model: Some("gpt-5.2"),
            top_models: &["gpt-5.2", "gpt-5.2-codex", "gpt-4o"],
            supports_api_url: false,
        }),
        "google" => Some(AiIntegrationSpec {
            provider: "gemini",
            requires_api_key: true,
            status_model_prefix: Some("google/"),
            default_model: Some("google/gemini-3.1-pro"),
            top_models: &[
                "google/gemini-3.1-pro",
                "google/gemini-3-flash",
                "google/gemini-2.5-pro",
            ],
            supports_api_url: false,
        }),
        "deepseek" => Some(AiIntegrationSpec {
            provider: "deepseek",
            requires_api_key: true,
            status_model_prefix: Some("deepseek/"),
            default_model: Some("deepseek/deepseek-reasoner"),
            top_models: &["deepseek/deepseek-reasoner", "deepseek/deepseek-chat"],
            supports_api_url: false,
        }),
        "xai" => Some(AiIntegrationSpec {
            provider: "xai",
            requires_api_key: true,
            status_model_prefix: Some("x-ai/"),
            default_model: Some("x-ai/grok-4"),
            top_models: &["x-ai/grok-4", "x-ai/grok-3"],
            supports_api_url: false,
        }),
        "mistral" => Some(AiIntegrationSpec {
            provider: "mistral",
            requires_api_key: true,
            status_model_prefix: Some("mistral"),
            default_model: Some("mistral-large-latest"),
            top_models: &[
                "mistral-large-latest",
                "codestral-latest",
                "mistral-small-latest",
            ],
            supports_api_url: false,
        }),
        "ollama" => Some(AiIntegrationSpec {
            provider: "ollama",
            requires_api_key: false,
            status_model_prefix: None,
            default_model: Some("llama3.2"),
            top_models: &[],
            supports_api_url: true,
        }),
        "perplexity" => Some(AiIntegrationSpec {
            provider: "perplexity",
            requires_api_key: true,
            status_model_prefix: None,
            default_model: Some("sonar-pro"),
            top_models: &["sonar-pro", "sonar-reasoning-pro", "sonar"],
            supports_api_url: false,
        }),
        "venice" => Some(AiIntegrationSpec {
            provider: "venice",
            requires_api_key: true,
            status_model_prefix: None,
            default_model: Some("venice-llama-3.3-70b"),
            top_models: &["venice-llama-3.3-70b"],
            supports_api_url: false,
        }),
        "vercel" => Some(AiIntegrationSpec {
            provider: "vercel",
            requires_api_key: true,
            status_model_prefix: None,
            default_model: Some("openai/gpt-5.2"),
            top_models: &[
                "openai/gpt-5.2",
                "anthropic/claude-sonnet-4-6",
                "google/gemini-3.1-pro",
            ],
            supports_api_url: false,
        }),
        "cloudflare" => Some(AiIntegrationSpec {
            provider: "cloudflare",
            requires_api_key: true,
            status_model_prefix: None,
            default_model: Some("@cf/meta/llama-3.3-70b-instruct-fp8-fast"),
            top_models: &[
                "@cf/meta/llama-3.3-70b-instruct-fp8-fast",
                "@cf/deepseek-ai/deepseek-r1-distill-qwen-32b",
            ],
            supports_api_url: false,
        }),
        "moonshot" => Some(AiIntegrationSpec {
            provider: "moonshot",
            requires_api_key: true,
            status_model_prefix: None,
            default_model: Some("moonshot-v1-128k"),
            top_models: &["moonshot-v1-128k", "kimi-k2.5", "kimi-for-coding"],
            supports_api_url: false,
        }),
        "synthetic" => Some(AiIntegrationSpec {
            provider: "synthetic",
            requires_api_key: true,
            status_model_prefix: None,
            default_model: Some("synthetic-1"),
            top_models: &["synthetic-1"],
            supports_api_url: false,
        }),
        "opencode" => Some(AiIntegrationSpec {
            provider: "opencode",
            requires_api_key: true,
            status_model_prefix: None,
            default_model: Some("opencode-zen"),
            top_models: &["opencode-zen"],
            supports_api_url: false,
        }),
        "zai" => Some(AiIntegrationSpec {
            provider: "zai",
            requires_api_key: true,
            status_model_prefix: None,
            default_model: Some("glm-4.7"),
            top_models: &["glm-4.7", "glm-4.5"],
            supports_api_url: false,
        }),
        "glm" => Some(AiIntegrationSpec {
            provider: "glm",
            requires_api_key: true,
            status_model_prefix: None,
            default_model: Some("glm-4.7"),
            top_models: &["glm-4.7", "glm-4.5"],
            supports_api_url: false,
        }),
        "minimax" => Some(AiIntegrationSpec {
            provider: "minimax",
            requires_api_key: true,
            status_model_prefix: None,
            default_model: Some("MiniMax-M1"),
            top_models: &["MiniMax-M1"],
            supports_api_url: false,
        }),
        "qwen" => Some(AiIntegrationSpec {
            provider: "qwen",
            requires_api_key: true,
            status_model_prefix: None,
            default_model: Some("qwen-max-latest"),
            top_models: &["qwen-max-latest", "qwen-plus-latest", "qwen-turbo-latest"],
            supports_api_url: false,
        }),
        "bedrock" => Some(AiIntegrationSpec {
            provider: "bedrock",
            requires_api_key: false,
            status_model_prefix: None,
            default_model: Some("anthropic.claude-sonnet-4-5-20250929-v1:0"),
            top_models: &[
                "anthropic.claude-sonnet-4-5-20250929-v1:0",
                "anthropic.claude-opus-4-6-v1:0",
            ],
            supports_api_url: false,
        }),
        "qianfan" => Some(AiIntegrationSpec {
            provider: "qianfan",
            requires_api_key: true,
            status_model_prefix: None,
            default_model: Some("ernie-4.0-8k-latest"),
            top_models: &["ernie-4.0-8k-latest"],
            supports_api_url: false,
        }),
        "groq" => Some(AiIntegrationSpec {
            provider: "groq",
            requires_api_key: true,
            status_model_prefix: None,
            default_model: Some("llama-3.3-70b-versatile"),
            top_models: &["llama-3.3-70b-versatile", "mixtral-8x7b-32768"],
            supports_api_url: false,
        }),
        "together" => Some(AiIntegrationSpec {
            provider: "together",
            requires_api_key: true,
            status_model_prefix: None,
            default_model: Some("meta-llama/Llama-3.3-70B-Instruct-Turbo"),
            top_models: &[
                "meta-llama/Llama-3.3-70B-Instruct-Turbo",
                "Qwen/Qwen2.5-72B-Instruct-Turbo",
                "deepseek-ai/DeepSeek-R1-Distill-Llama-70B",
            ],
            supports_api_url: false,
        }),
        "fireworks" => Some(AiIntegrationSpec {
            provider: "fireworks",
            requires_api_key: true,
            status_model_prefix: None,
            default_model: Some("accounts/fireworks/models/llama-v3p1-8b-instruct"),
            top_models: &[
                "accounts/fireworks/models/llama-v3p1-8b-instruct",
                "accounts/fireworks/models/deepseek-v3",
            ],
            supports_api_url: false,
        }),
        "cohere" => Some(AiIntegrationSpec {
            provider: "cohere",
            requires_api_key: true,
            status_model_prefix: None,
            default_model: Some("command-r-plus-08-2024"),
            top_models: &["command-r-plus-08-2024", "command-r-08-2024"],
            supports_api_url: false,
        }),
        _ => None,
    }
}

fn is_ai_integration_id(integration_id: &str) -> bool {
    ai_integration_spec(integration_id).is_some()
}

fn normalize_provider_match_name(provider: Option<&str>) -> Option<String> {
    let raw = provider?.trim();
    if raw.is_empty() {
        return None;
    }

    let base = if raw.starts_with("custom:") || raw.starts_with("anthropic-custom:") {
        raw
    } else {
        match raw.split_once(':') {
            Some((provider_name, profile)) if !profile.is_empty() => provider_name,
            _ => raw,
        }
    };

    Some(base.to_ascii_lowercase())
}

fn ai_provider_matches_integration(integration_id: &str, provider: Option<&str>) -> bool {
    let Some(provider) = normalize_provider_match_name(provider) else {
        return false;
    };
    let provider = provider.as_str();

    match integration_id {
        "openai" => {
            provider == "openai"
                || provider == "openai-codex"
                || provider == "openai_codex"
                || provider == "codex"
        }
        "google" => provider == "gemini" || provider == "google",
        "xai" => provider == "xai" || provider == "grok",
        "moonshot" => {
            is_moonshot_alias(provider)
                || provider == "kimi-code"
                || provider == "kimi_coding"
                || provider == "kimi_for_coding"
        }
        "glm" => is_glm_alias(provider),
        "minimax" => is_minimax_alias(provider),
        "qwen" => is_qwen_alias(provider),
        "zai" => is_zai_alias(provider),
        "qianfan" => is_qianfan_alias(provider),
        "bedrock" => provider == "bedrock" || provider == "aws-bedrock",
        "vercel" => provider == "vercel" || provider == "vercel-ai",
        "cloudflare" => provider == "cloudflare" || provider == "cloudflare-ai",
        "opencode" => provider == "opencode" || provider == "opencode-zen",
        _ => provider == integration_id,
    }
}

fn active_ai_integration_id(provider: Option<&str>) -> Option<&'static str> {
    let normalized = normalize_provider_match_name(provider)?;

    crate::integrations::registry::all_integrations()
        .iter()
        .filter_map(|entry| integration_id_from_name(entry.name))
        .find(|integration_id| {
            is_ai_integration_id(integration_id)
                && ai_provider_matches_integration(integration_id, Some(normalized.as_str()))
        })
}

fn integration_fields(
    integration_id: &str,
    config: &crate::config::Config,
) -> (bool, Vec<IntegrationCredentialsField>) {
    match integration_id {
        "telegram" => {
            let token_present = config
                .channels_config
                .telegram
                .as_ref()
                .and_then(|channel| non_empty_trimmed(&channel.bot_token))
                .is_some();

            (
                token_present,
                vec![IntegrationCredentialsField {
                    key: "bot_token",
                    label: "Bot Token",
                    required: true,
                    has_value: token_present,
                    input_type: "secret",
                    options: Vec::new(),
                    current_value: None,
                    masked_value: mask_secret_value(token_present),
                }],
            )
        }
        "discord" => {
            let token_present = config
                .channels_config
                .discord
                .as_ref()
                .and_then(|channel| non_empty_trimmed(&channel.bot_token))
                .is_some();

            (
                token_present,
                vec![IntegrationCredentialsField {
                    key: "bot_token",
                    label: "Bot Token",
                    required: true,
                    has_value: token_present,
                    input_type: "secret",
                    options: Vec::new(),
                    current_value: None,
                    masked_value: mask_secret_value(token_present),
                }],
            )
        }
        "slack" => {
            let bot_token_present = config
                .channels_config
                .slack
                .as_ref()
                .and_then(|channel| non_empty_trimmed(&channel.bot_token))
                .is_some();
            let app_token_present = config
                .channels_config
                .slack
                .as_ref()
                .and_then(|channel| channel.app_token.as_deref())
                .and_then(non_empty_trimmed)
                .is_some();

            (
                bot_token_present,
                vec![
                    IntegrationCredentialsField {
                        key: "bot_token",
                        label: "Bot Token",
                        required: true,
                        has_value: bot_token_present,
                        input_type: "secret",
                        options: Vec::new(),
                        current_value: None,
                        masked_value: mask_secret_value(bot_token_present),
                    },
                    IntegrationCredentialsField {
                        key: "app_token",
                        label: "App Token",
                        required: false,
                        has_value: app_token_present,
                        input_type: "secret",
                        options: Vec::new(),
                        current_value: None,
                        masked_value: mask_secret_value(app_token_present),
                    },
                ],
            )
        }
        _ => {
            if let Some(spec) = ai_integration_spec(integration_id) {
                let provider_matches = ai_provider_matches_integration(
                    integration_id,
                    config.default_provider.as_deref(),
                );
                let active_provider =
                    normalize_provider_match_name(config.default_provider.as_deref());
                let api_key_present = provider_matches
                    && active_provider.as_deref().is_some_and(|provider| {
                        provider_credential_available(provider, config.api_key.as_deref())
                    });
                let default_model_present = provider_matches
                    && config
                        .default_model
                        .as_deref()
                        .and_then(non_empty_trimmed)
                        .is_some();
                let api_url_present = provider_matches
                    && config
                        .api_url
                        .as_deref()
                        .and_then(non_empty_trimmed)
                        .is_some();

                let mut fields = Vec::new();
                fields.push(IntegrationCredentialsField {
                    key: "api_key",
                    label: "API Key",
                    required: spec.requires_api_key,
                    has_value: api_key_present,
                    input_type: "secret",
                    options: Vec::new(),
                    current_value: None,
                    masked_value: mask_secret_value(api_key_present),
                });
                fields.push(IntegrationCredentialsField {
                    key: "default_model",
                    label: "Default Model",
                    required: false,
                    has_value: default_model_present,
                    input_type: if spec.top_models.is_empty() {
                        "text"
                    } else {
                        "select"
                    },
                    options: spec.top_models.to_vec(),
                    current_value: if provider_matches {
                        config
                            .default_model
                            .as_deref()
                            .and_then(non_empty_trimmed)
                            .map(str::to_owned)
                    } else {
                        None
                    },
                    masked_value: None,
                });

                if spec.supports_api_url {
                    fields.push(IntegrationCredentialsField {
                        key: "api_url",
                        label: "API URL",
                        required: false,
                        has_value: api_url_present,
                        input_type: "text",
                        options: Vec::new(),
                        current_value: if provider_matches {
                            config
                                .api_url
                                .as_deref()
                                .and_then(non_empty_trimmed)
                                .map(str::to_owned)
                        } else {
                            None
                        },
                        masked_value: None,
                    });
                }

                let configured = if !provider_matches {
                    false
                } else if spec.requires_api_key {
                    api_key_present
                } else {
                    true
                };

                (configured, fields)
            } else {
                (false, Vec::new())
            }
        }
    }
}

fn mask_secret_value(has_value: bool) -> Option<&'static str> {
    has_value.then_some("••••••••")
}

fn non_empty_trimmed(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

enum OptionalSecretUpdate {
    Unchanged,
    Clear,
    Set(String),
}

fn parse_optional_secret_input(
    fields: &HashMap<String, String>,
    key: &str,
) -> OptionalSecretUpdate {
    match fields.get(key) {
        None => OptionalSecretUpdate::Unchanged,
        Some(value) => match non_empty_trimmed(value) {
            Some(trimmed) => OptionalSecretUpdate::Set(trimmed.to_owned()),
            None => OptionalSecretUpdate::Clear,
        },
    }
}

fn require_secret_input(
    fields: &HashMap<String, String>,
    key: &str,
    existing_value: Option<&str>,
) -> Result<String, String> {
    if let Some(value) = fields.get(key).and_then(|value| non_empty_trimmed(value)) {
        return Ok(value.to_owned());
    }

    if let Some(value) = existing_value.and_then(non_empty_trimmed) {
        return Ok(value.to_owned());
    }

    Err(format!("Missing required field: {key}"))
}

fn validate_allowed_fields(
    integration_id: &str,
    fields: &HashMap<String, String>,
) -> Result<(), String> {
    let ai_spec = ai_integration_spec(integration_id);
    let is_allowed = |key: &str| match integration_id {
        "telegram" | "discord" => key == "bot_token",
        "slack" => key == "bot_token" || key == "app_token",
        _ => {
            if let Some(spec) = ai_spec {
                key == "api_key"
                    || key == "default_model"
                    || (spec.supports_api_url && key == "api_url")
            } else {
                false
            }
        }
    };

    if !matches!(integration_id, "telegram" | "discord" | "slack") && ai_spec.is_none() {
        return Err(format!(
            "Integration '{integration_id}' does not support manual credential updates"
        ));
    }

    if let Some(unknown_key) = fields.keys().find(|key| !is_allowed(key.as_str())) {
        return Err(format!(
            "Unknown field '{unknown_key}' for integration '{integration_id}'"
        ));
    }

    Ok(())
}

fn apply_integration_credentials(
    config: &mut crate::config::Config,
    integration_id: &str,
    fields: &HashMap<String, String>,
) -> Result<(), String> {
    validate_allowed_fields(integration_id, fields)?;

    match integration_id {
        "telegram" => {
            let existing_token = config
                .channels_config
                .telegram
                .as_ref()
                .map(|channel| channel.bot_token.as_str());
            let bot_token = require_secret_input(fields, "bot_token", existing_token)?;

            if let Some(channel) = config.channels_config.telegram.as_mut() {
                channel.bot_token = bot_token;
            } else {
                config.channels_config.telegram = Some(crate::config::TelegramConfig {
                    bot_token,
                    allowed_users: Vec::new(),
                    stream_mode: crate::config::StreamMode::default(),
                    draft_update_interval_ms: 1000,
                    interrupt_on_new_message: false,
                    mention_only: false,
                });
            }
        }
        "discord" => {
            let existing_token = config
                .channels_config
                .discord
                .as_ref()
                .map(|channel| channel.bot_token.as_str());
            let bot_token = require_secret_input(fields, "bot_token", existing_token)?;

            if let Some(channel) = config.channels_config.discord.as_mut() {
                channel.bot_token = bot_token;
            } else {
                config.channels_config.discord = Some(crate::config::DiscordConfig {
                    bot_token,
                    guild_id: None,
                    allowed_users: Vec::new(),
                    listen_to_bots: false,
                    mention_only: false,
                });
            }
        }
        "slack" => {
            let existing_bot_token = config
                .channels_config
                .slack
                .as_ref()
                .map(|channel| channel.bot_token.as_str());
            let bot_token = require_secret_input(fields, "bot_token", existing_bot_token)?;
            let app_token_update = parse_optional_secret_input(fields, "app_token");

            if let Some(channel) = config.channels_config.slack.as_mut() {
                channel.bot_token = bot_token;
                match app_token_update {
                    OptionalSecretUpdate::Unchanged => {}
                    OptionalSecretUpdate::Clear => channel.app_token = None,
                    OptionalSecretUpdate::Set(app_token) => channel.app_token = Some(app_token),
                }
            } else {
                config.channels_config.slack = Some(crate::config::SlackConfig {
                    bot_token,
                    app_token: match app_token_update {
                        OptionalSecretUpdate::Unchanged | OptionalSecretUpdate::Clear => None,
                        OptionalSecretUpdate::Set(app_token) => Some(app_token),
                    },
                    channel_id: None,
                    allowed_users: Vec::new(),
                });
            }
        }
        _ => {
            if let Some(spec) = ai_integration_spec(integration_id) {
                let force_default_model = matches!(integration_id, "ollama" | "bedrock");
                config.default_provider = Some(spec.provider.to_owned());

                let api_key_update = parse_optional_secret_input(fields, "api_key");
                match api_key_update {
                    OptionalSecretUpdate::Unchanged => {}
                    OptionalSecretUpdate::Clear => config.api_key = None,
                    OptionalSecretUpdate::Set(api_key) => config.api_key = Some(api_key),
                }

                if spec.requires_api_key
                    && !provider_credential_available(spec.provider, config.api_key.as_deref())
                {
                    return Err("Missing required field: api_key".to_string());
                }

                let default_model_update = parse_optional_secret_input(fields, "default_model");
                let default_model_unchanged =
                    matches!(&default_model_update, OptionalSecretUpdate::Unchanged);
                match default_model_update {
                    OptionalSecretUpdate::Unchanged => {}
                    OptionalSecretUpdate::Clear => config.default_model = None,
                    OptionalSecretUpdate::Set(model) => config.default_model = Some(model),
                }

                if let Some(prefix) = spec.status_model_prefix {
                    let matches_prefix = config
                        .default_model
                        .as_deref()
                        .is_some_and(|model| model.starts_with(prefix));
                    if !matches_prefix {
                        if let Some(default_model) = spec.default_model {
                            config.default_model = Some(default_model.to_owned());
                        }
                    }
                } else if default_model_unchanged
                    && (config.default_model.is_none() || force_default_model)
                {
                    if let Some(default_model) = spec.default_model {
                        config.default_model = Some(default_model.to_owned());
                    }
                }

                if spec.supports_api_url {
                    match parse_optional_secret_input(fields, "api_url") {
                        OptionalSecretUpdate::Unchanged => {}
                        OptionalSecretUpdate::Clear => config.api_url = None,
                        OptionalSecretUpdate::Set(api_url) => config.api_url = Some(api_url),
                    }
                }
            } else {
                return Err(format!(
                    "Integration '{integration_id}' does not support manual credential updates"
                ));
            }
        }
    }

    Ok(())
}

fn mask_sensitive_fields(toml_str: &str) -> String {
    let mut output = String::with_capacity(toml_str.len());
    for line in toml_str.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("api_key")
            || trimmed.starts_with("bot_token")
            || trimmed.starts_with("access_token")
            || trimmed.starts_with("secret")
            || trimmed.starts_with("app_secret")
            || trimmed.starts_with("signing_secret")
        {
            if let Some(eq_pos) = line.find('=') {
                output.push_str(&line[..=eq_pos]);
                output.push_str(" \"***MASKED***\"");
            } else {
                output.push_str(line);
            }
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, original }
        }

        fn unset(key: &'static str) -> Self {
            let original = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.original.as_ref() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn apply_integration_credentials_creates_telegram_config_when_missing() {
        let mut config = crate::config::Config::default();
        assert!(config.channels_config.telegram.is_none());

        let mut fields = HashMap::new();
        fields.insert("bot_token".to_string(), "123:token".to_string());

        apply_integration_credentials(&mut config, "telegram", &fields).unwrap();

        let telegram = config
            .channels_config
            .telegram
            .as_ref()
            .expect("telegram config should be created");
        assert_eq!(telegram.bot_token, "123:token");
    }

    #[test]
    fn apply_integration_credentials_rejects_unknown_field() {
        let mut config = crate::config::Config::default();
        let mut fields = HashMap::new();
        fields.insert("unknown".to_string(), "value".to_string());

        let err = apply_integration_credentials(&mut config, "discord", &fields).unwrap_err();
        assert!(err.contains("Unknown field"));
    }

    #[test]
    fn apply_integration_credentials_preserves_required_secret_when_omitted() {
        let mut config = crate::config::Config::default();
        config.channels_config.discord = Some(crate::config::DiscordConfig {
            bot_token: "discord-token".to_string(),
            guild_id: None,
            allowed_users: Vec::new(),
            listen_to_bots: false,
            mention_only: false,
        });

        let fields = HashMap::new();
        apply_integration_credentials(&mut config, "discord", &fields).unwrap();

        let discord = config.channels_config.discord.as_ref().unwrap();
        assert_eq!(discord.bot_token, "discord-token");
    }

    #[test]
    fn apply_integration_credentials_allows_clearing_optional_slack_app_token() {
        let mut config = crate::config::Config::default();
        config.channels_config.slack = Some(crate::config::SlackConfig {
            bot_token: "xoxb-existing".to_string(),
            app_token: Some("xapp-existing".to_string()),
            channel_id: None,
            allowed_users: Vec::new(),
        });

        let mut fields = HashMap::new();
        fields.insert("app_token".to_string(), "   ".to_string());
        apply_integration_credentials(&mut config, "slack", &fields).unwrap();

        let slack = config.channels_config.slack.as_ref().unwrap();
        assert_eq!(slack.bot_token, "xoxb-existing");
        assert!(slack.app_token.is_none());
    }

    #[test]
    fn apply_integration_credentials_sets_ai_provider_and_key() {
        let mut config = crate::config::Config::default();
        let previous_model = config.default_model.clone();
        let mut fields = HashMap::new();
        fields.insert("api_key".to_string(), "sk-openrouter".to_string());

        apply_integration_credentials(&mut config, "openrouter", &fields).unwrap();

        assert_eq!(config.default_provider.as_deref(), Some("openrouter"));
        assert_eq!(config.api_key.as_deref(), Some("sk-openrouter"));
        assert_eq!(config.default_model, previous_model);
    }

    #[test]
    fn apply_integration_credentials_sets_google_default_model_for_status_prefix() {
        let mut config = crate::config::Config::default();
        config.api_key = Some("sk-google".to_string());
        config.default_model = Some("gpt-4o-mini".to_string());

        let fields = HashMap::new();
        apply_integration_credentials(&mut config, "google", &fields).unwrap();

        assert_eq!(config.default_provider.as_deref(), Some("gemini"));
        assert_eq!(
            config.default_model.as_deref(),
            Some("google/gemini-3.1-pro")
        );
    }

    #[test]
    fn apply_integration_credentials_allows_bedrock_without_api_key() {
        let mut config = crate::config::Config::default();

        let fields = HashMap::new();
        apply_integration_credentials(&mut config, "bedrock", &fields).unwrap();

        assert_eq!(config.default_provider.as_deref(), Some("bedrock"));
        assert_eq!(
            config.default_model.as_deref(),
            Some("anthropic.claude-sonnet-4-5-20250929-v1:0")
        );
    }

    #[test]
    fn apply_integration_credentials_rejects_unknown_ai_field() {
        let mut config = crate::config::Config::default();
        let mut fields = HashMap::new();
        fields.insert("bot_token".to_string(), "value".to_string());

        let err = apply_integration_credentials(&mut config, "openai", &fields).unwrap_err();
        assert!(err.contains("Unknown field"));
    }

    #[test]
    fn config_revision_changes_after_secret_update() {
        let mut config = crate::config::Config::default();
        let before = config_revision(&config).unwrap();

        let mut fields = HashMap::new();
        fields.insert("bot_token".to_string(), "123:token".to_string());
        apply_integration_credentials(&mut config, "telegram", &fields).unwrap();

        let after = config_revision(&config).unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn integration_fields_scope_ai_values_to_active_provider_only() {
        let mut config = crate::config::Config::default();
        config.default_provider = Some("openrouter".to_string());
        config.api_key = Some("sk-openrouter".to_string());
        config.default_model = Some("anthropic/claude-sonnet-4-6".to_string());

        let (openrouter_configured, openrouter_fields) = integration_fields("openrouter", &config);
        assert!(openrouter_configured);
        let openrouter_model = openrouter_fields
            .iter()
            .find(|field| field.key == "default_model")
            .expect("openrouter default_model field");
        assert!(openrouter_model.has_value);
        assert_eq!(
            openrouter_model.current_value.as_deref(),
            Some("anthropic/claude-sonnet-4-6")
        );

        let (openai_configured, openai_fields) = integration_fields("openai", &config);
        assert!(!openai_configured);
        let openai_key = openai_fields
            .iter()
            .find(|field| field.key == "api_key")
            .expect("openai api_key field");
        assert!(!openai_key.has_value);
        let openai_model = openai_fields
            .iter()
            .find(|field| field.key == "default_model")
            .expect("openai default_model field");
        assert!(!openai_model.has_value);
        assert!(openai_model.current_value.is_none());
    }

    #[test]
    fn integration_fields_treats_provider_alias_as_active_match() {
        let mut config = crate::config::Config::default();
        config.default_provider = Some("google".to_string());
        config.api_key = Some("sk-google".to_string());
        config.default_model = Some("google/gemini-3.1-pro".to_string());

        let (configured, fields) = integration_fields("google", &config);
        assert!(configured);

        let model = fields
            .iter()
            .find(|field| field.key == "default_model")
            .expect("google default_model field");
        assert!(model.has_value);
        assert_eq!(
            model.current_value.as_deref(),
            Some("google/gemini-3.1-pro")
        );
    }

    #[test]
    fn integration_fields_treat_provider_profile_as_active_match() {
        let mut config = crate::config::Config::default();
        config.default_provider = Some("openrouter:work".to_string());
        config.api_key = Some("sk-openrouter".to_string());
        config.default_model = Some("anthropic/claude-sonnet-4-6".to_string());

        let (configured, fields) = integration_fields("openrouter", &config);
        assert!(configured);

        let api_key = fields
            .iter()
            .find(|field| field.key == "api_key")
            .expect("openrouter api_key field");
        assert!(api_key.has_value);
    }

    #[test]
    fn integration_fields_non_active_ollama_not_marked_configured() {
        let mut config = crate::config::Config::default();
        config.default_provider = Some("openrouter".to_string());
        config.default_model = Some("anthropic/claude-sonnet-4-6".to_string());
        config.api_url = Some("http://127.0.0.1:11434/v1".to_string());

        let (configured, fields) = integration_fields("ollama", &config);
        assert!(!configured);

        let api_url = fields
            .iter()
            .find(|field| field.key == "api_url")
            .expect("ollama api_url field");
        assert!(!api_url.has_value);
        assert!(api_url.current_value.is_none());
    }

    #[test]
    fn ai_provider_matching_supports_runtime_aliases() {
        assert!(ai_provider_matches_integration("xai", Some("grok")));
        assert!(ai_provider_matches_integration("google", Some("google")));
        assert!(ai_provider_matches_integration(
            "bedrock",
            Some("aws-bedrock")
        ));
        assert!(ai_provider_matches_integration(
            "moonshot",
            Some("kimi-intl")
        ));
        assert!(ai_provider_matches_integration(
            "moonshot",
            Some("kimi-code")
        ));
        assert!(ai_provider_matches_integration(
            "openai",
            Some("openai-codex")
        ));
        assert!(ai_provider_matches_integration(
            "opencode",
            Some("opencode-zen")
        ));
        assert!(!ai_provider_matches_integration(
            "openai",
            Some("anthropic")
        ));
    }

    #[test]
    fn active_ai_integration_id_resolves_aliases_to_integration_id() {
        assert_eq!(active_ai_integration_id(Some("google")), Some("google"));
        assert_eq!(
            active_ai_integration_id(Some("openai-codex")),
            Some("openai")
        );
        assert_eq!(
            active_ai_integration_id(Some("aws-bedrock")),
            Some("bedrock")
        );
        assert_eq!(
            active_ai_integration_id(Some("openrouter:work")),
            Some("openrouter")
        );
    }

    #[test]
    fn active_ai_integration_id_returns_none_for_unknown_provider() {
        assert_eq!(active_ai_integration_id(Some("not-a-provider")), None);
        assert_eq!(active_ai_integration_id(None), None);
    }

    #[test]
    fn integration_fields_expose_masked_secret_value_for_configured_fields() {
        let mut config = crate::config::Config::default();
        config.default_provider = Some("openrouter".to_string());
        config.api_key = Some("sk-openrouter".to_string());

        let (_configured, fields) = integration_fields("openrouter", &config);
        let api_key_field = fields
            .iter()
            .find(|field| field.key == "api_key")
            .expect("api_key field");

        assert_eq!(api_key_field.masked_value, Some("••••••••"));
    }

    #[test]
    fn integration_fields_considers_provider_env_api_key_as_configured() {
        let _lock = env_lock();
        let _generic = EnvGuard::unset("ZEROCLAW_API_KEY");
        let _api_key = EnvGuard::unset("API_KEY");
        let _provider_key = EnvGuard::set("OPENROUTER_API_KEY", "sk-openrouter-env");

        let mut config = crate::config::Config::default();
        config.default_provider = Some("openrouter".to_string());
        config.api_key = None;

        let (configured, fields) = integration_fields("openrouter", &config);
        assert!(configured);

        let api_key_field = fields
            .iter()
            .find(|field| field.key == "api_key")
            .expect("api_key field");
        assert!(api_key_field.has_value);
        assert_eq!(api_key_field.masked_value, Some("••••••••"));
    }

    #[test]
    fn integration_fields_considers_google_env_keys_as_configured() {
        let _lock = env_lock();
        let _generic = EnvGuard::unset("ZEROCLAW_API_KEY");
        let _api_key = EnvGuard::unset("API_KEY");
        let _gemini_key = EnvGuard::unset("GEMINI_API_KEY");
        let _google_key = EnvGuard::set("GOOGLE_API_KEY", "sk-google-env");

        let mut config = crate::config::Config::default();
        config.default_provider = Some("gemini".to_string());
        config.api_key = None;

        let (configured, fields) = integration_fields("google", &config);
        assert!(configured);

        let api_key_field = fields
            .iter()
            .find(|field| field.key == "api_key")
            .expect("api_key field");
        assert!(api_key_field.has_value);
        assert_eq!(api_key_field.masked_value, Some("••••••••"));
    }

    #[test]
    fn apply_integration_credentials_allows_ai_update_without_persisted_key_when_env_has_key() {
        let _lock = env_lock();
        let _generic = EnvGuard::unset("ZEROCLAW_API_KEY");
        let _api_key = EnvGuard::unset("API_KEY");
        let _provider_key = EnvGuard::set("OPENROUTER_API_KEY", "sk-openrouter-env");

        let mut config = crate::config::Config::default();
        config.default_provider = Some("openrouter".to_string());
        config.api_key = None;

        let mut fields = HashMap::new();
        fields.insert(
            "default_model".to_string(),
            "anthropic/claude-sonnet-4-6".to_string(),
        );

        apply_integration_credentials(&mut config, "openrouter", &fields).unwrap();

        assert_eq!(config.default_provider.as_deref(), Some("openrouter"));
        assert!(config.api_key.is_none());
        assert_eq!(
            config.default_model.as_deref(),
            Some("anthropic/claude-sonnet-4-6")
        );
    }
}
