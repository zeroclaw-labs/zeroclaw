//! REST API handlers for the web dashboard.
//!
//! All `/api/*` routes require bearer token authentication (PairingGuard).

use super::AppState;
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};
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
        mask_optional_secret(&mut wati.webhook_secret);
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
        restore_optional_secret(&mut incoming_ch.webhook_secret, &current_ch.webhook_secret);
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
            webhook_secret: Some("wati-hook-secret".to_string()),
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
                .wati
                .as_ref()
                .and_then(|value| value.webhook_secret.as_deref()),
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
            webhook_secret: Some("wati-hook-secret".to_string()),
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
                .wati
                .as_ref()
                .and_then(|value| value.webhook_secret.as_deref()),
            Some("wati-hook-secret")
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
