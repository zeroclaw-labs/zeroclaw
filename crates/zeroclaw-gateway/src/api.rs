//! REST API handlers for the web dashboard.
//! All `/api/*` routes require bearer token authentication (PairingGuard).

use super::AppState;
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use zeroclaw_config::schema::{ChannelAliasInfo, Config};
use zeroclaw_memory::MemoryEntry;

const MEMORY_API_CONTENT_MAX_CHARS: usize = 4096;

fn integration_entry_json(
    entry: &zeroclaw_runtime::integrations::IntegrationEntry,
) -> serde_json::Value {
    serde_json::json!({
        "name": &entry.name,
        "description": &entry.description,
        "category": entry.category,
        "category_label": entry.category.label(),
        "status": entry.status,
    })
}

// ── Bearer token auth extractor ─────────────────────────────────

/// Extract and validate bearer token from Authorization header.
pub(crate) fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
}

/// Verify bearer token against PairingGuard. Returns error response if unauthorized.
pub(crate) fn require_auth(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if !state.pairing.require_pairing() {
        return Ok(());
    }

    let token = extract_bearer_token(headers).unwrap_or("");
    // Defense-in-depth: reject empty tokens explicitly so a future
    // refactor of is_authenticated cannot accidentally treat "" as valid.
    if token.is_empty() {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>"
            })),
        ));
    }
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
    /// Filter memories created at or after (RFC 3339 / ISO 8601)
    pub since: Option<String>,
    /// Filter memories created at or before (RFC 3339 / ISO 8601)
    pub until: Option<String>,
    /// When set to a configured agent alias, the request goes through
    /// that agent's per-alias memory backend (so SQL backends filter by
    /// the agent's UUID, Markdown reads only that agent's directory,
    /// etc.). Omit for the install-wide view.
    #[serde(default)]
    pub agent: Option<String>,
}

#[derive(Deserialize)]
pub struct MemoryStoreBody {
    pub key: String,
    pub content: String,
    pub category: Option<String>,
    /// Configured agent alias to write under. When omitted the store goes
    /// to the install-wide memory backend (no per-agent attribution).
    #[serde(default)]
    pub agent: Option<String>,
}

#[derive(Deserialize)]
pub struct MemoryDeleteQuery {
    /// Configured agent alias to delete from. Omit for the install-wide
    /// backend.
    #[serde(default)]
    pub agent: Option<String>,
}

#[derive(Deserialize)]
pub struct CronRunsQuery {
    pub limit: Option<u32>,
}

#[derive(Deserialize)]
pub struct CronAddBody {
    /// Configured agent alias the cron job will run as. Required —
    /// there is no default agent.
    pub agent: String,
    pub name: Option<String>,
    pub schedule: String,
    pub tz: Option<String>,
    pub command: Option<String>,
    pub job_type: Option<String>,
    pub prompt: Option<String>,
    pub delivery: Option<zeroclaw_runtime::cron::DeliveryConfig>,
    pub session_target: Option<String>,
    pub model: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
    pub delete_after_run: Option<bool>,
    /// If false, disable memory recall for this agent cron job (default: true).
    pub uses_memory: Option<bool>,
}

#[derive(Deserialize)]
pub struct CronPatchBody {
    /// Configured agent alias whose risk profile gates the new shell
    /// command. Only consulted when `command` is being patched; optional
    /// otherwise (e.g. a pure schedule/name change or an enable/disable
    /// toggle), so non-command patches need not supply it.
    #[serde(default)]
    pub agent: String,
    pub name: Option<String>,
    pub schedule: Option<String>,
    pub tz: Option<String>,
    pub clear_tz: Option<bool>,
    pub command: Option<String>,
    pub prompt: Option<String>,
    /// Toggle the job on/off without deleting it (pause/resume). `None` leaves
    /// the current state unchanged.
    pub enabled: Option<bool>,
    /// If false, disable memory recall for this agent cron job (default: true).
    pub uses_memory: Option<bool>,
}

enum CronTimezonePatch {
    Preserve,
    Set(String),
    Clear,
}

fn bad_request(message: impl Into<String>) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message.into() })),
    )
}

fn normalize_optional_timezone(
    tz: Option<String>,
) -> Result<Option<String>, (StatusCode, Json<serde_json::Value>)> {
    match tz {
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                Err(bad_request(
                    "tz must be a non-empty IANA timezone; use clear_tz=true to clear it",
                ))
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        None => Ok(None),
    }
}

fn parse_timezone_patch(
    tz: Option<String>,
    clear_tz: Option<bool>,
) -> Result<CronTimezonePatch, (StatusCode, Json<serde_json::Value>)> {
    let tz = normalize_optional_timezone(tz)?;
    let clear_tz = clear_tz.unwrap_or(false);

    if clear_tz && tz.is_some() {
        return Err(bad_request("Provide either tz or clear_tz=true, not both"));
    }

    if clear_tz {
        Ok(CronTimezonePatch::Clear)
    } else if let Some(tz) = tz {
        Ok(CronTimezonePatch::Set(tz))
    } else {
        Ok(CronTimezonePatch::Preserve)
    }
}

fn cron_schedule_from_api(
    expr: String,
    tz: Option<String>,
) -> Result<zeroclaw_runtime::cron::Schedule, (StatusCode, Json<serde_json::Value>)> {
    let schedule = zeroclaw_runtime::cron::Schedule::Cron { expr, tz };
    zeroclaw_runtime::cron::validate_schedule(&schedule, chrono::Utc::now())
        .map_err(|e| bad_request(format!("Invalid cron schedule: {e}")))?;
    Ok(schedule)
}

#[derive(Deserialize)]
pub struct SessionMessagePostBody {
    pub content: String,
}

// ── Handlers ────────────────────────────────────────────────────

/// Query parameters for `GET /api/status`. Pass `?agent=<alias>` to
/// have `model_provider`, `model`, `temperature`, and `memory_backend`
/// reflect that specific agent's resolved config; omit it for the
/// install-wide summary.
#[derive(Debug, Deserialize)]
pub struct StatusQuery {
    #[serde(default)]
    pub agent: Option<String>,
}

/// GET /api/status — system status overview
pub async fn handle_api_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<StatusQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.read().clone();
    let health = zeroclaw_runtime::health::snapshot();

    // Per-alias map keyed by composite `<type>.<alias>`. Every
    // populated `[channels.<type>.<alias>]` is a separate dashboard row.
    let mut channels = serde_json::Map::new();
    for info in config.channels_by_alias() {
        let composite = format!("{}.{}", info.channel_type, info.alias);
        channels.insert(composite, serde_json::Value::Bool(true));
    }

    let locale = config
        .locale
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(zeroclaw_runtime::i18n::detect_locale);

    // Per-agent resolution when `?agent=<alias>` is supplied. Falls back
    // to the install-wide first-of-each view when the alias is unknown
    // (so the dashboard's old shape still renders during onboarding,
    // before any agent exists).
    let agent_alias = query.agent.as_deref().filter(|s| !s.trim().is_empty());
    let (model_provider, model, temperature, memory_backend) =
        match agent_alias.and_then(|alias| config.agent(alias).map(|a| (alias, a))) {
            Some((alias, agent)) => {
                let provider_ref = if agent.model_provider.is_empty() {
                    None
                } else {
                    Some(agent.model_provider.as_str().to_string())
                };
                let resolved = config.resolved_model_provider_for_agent(alias);
                let model = resolved
                    .as_ref()
                    .and_then(|(_, _, cfg)| cfg.model.clone())
                    .unwrap_or_default();
                let temperature: Option<f64> =
                    resolved.as_ref().and_then(|(_, _, cfg)| cfg.temperature);
                let backend_kind = agent.memory.backend;
                let backend = serde_json::to_value(backend_kind)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_else(|| format!("{backend_kind:?}").to_lowercase());
                (provider_ref, model, temperature, backend)
            }
            None => (
                config
                    .providers
                    .models
                    .iter_entries()
                    .next()
                    .map(|(ty, alias, _)| format!("{ty}.{alias}")),
                state.model.clone(),
                state.temperature,
                state.mem.name().to_string(),
            ),
        };

    let process = zeroclaw_runtime::process_stats::sample();

    // Upgrade affordance: whether the dashboard should poll for updates / offer
    // the upgrade button, and which restart command to show afterwards.
    let restart = crate::version::detect_restart();

    let body = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "model_provider": model_provider,
        "model": model,
        "temperature": temperature,
        "uptime_seconds": health.uptime_seconds,
        "daemon_started_at": zeroclaw_runtime::health::daemon_started_at(),
        "gateway_port": config.gateway.port,
        "locale": locale,
        "memory_backend": memory_backend,
        "paired": state.pairing.is_paired(),
        "channels": channels,
        "health": health,
        "agent_alias": agent_alias,
        "process": process,
        "check_updates": config.gateway.check_updates,
        "allow_self_upgrade": config.gateway.allow_self_upgrade,
        "restart_mode": restart.mode.as_str(),
        "restart_hint": restart.hint,
    });

    Json(body).into_response()
}

#[derive(Debug, Deserialize)]
pub struct ToolsQuery {
    #[serde(default)]
    pub agent: Option<String>,
}

/// GET /api/tools - list registered tool specs, optionally scoped to `?agent=`
pub async fn handle_api_tools(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ToolsQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let registry = query
        .agent
        .as_deref()
        .map(str::trim)
        .filter(|alias| !alias.is_empty())
        .and_then(|alias| state.tools_registry_by_agent.get(alias).cloned())
        .unwrap_or_else(|| std::sync::Arc::clone(&state.tools_registry));

    let tools: Vec<serde_json::Value> = registry
        .iter()
        .map(|spec| {
            let mut tool = serde_json::json!({
                "name": spec.name,
                "description": spec.description,
                "parameters": spec.parameters,
            });
            if let Some(output) = &spec.output {
                tool["output"] = output.clone();
            }
            if !spec.param_domains.is_empty() {
                tool["param_domains"] = serde_json::json!(spec.param_domains);
            }
            tool
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

    let config = state.config.read().clone();
    match zeroclaw_runtime::cron::list_jobs(&config) {
        Ok(jobs) => Json(serde_json::json!({"jobs": jobs})).into_response(),
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

    let CronAddBody {
        agent: agent_alias,
        name,
        schedule,
        tz,
        command,
        job_type,
        prompt,
        delivery,
        session_target,
        model,
        allowed_tools,
        delete_after_run,
        uses_memory,
    } = body;

    let config = state.config.read().clone();
    if config.agent(&agent_alias).is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!(
                "Unknown agent {agent_alias:?} (no [agents.{agent_alias}] entry configured)"
            )})),
        )
            .into_response();
    }
    let tz = match normalize_optional_timezone(tz) {
        Ok(tz) => tz,
        Err(e) => return e.into_response(),
    };
    let schedule = match cron_schedule_from_api(schedule, tz) {
        Ok(schedule) => schedule,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = zeroclaw_runtime::cron::validate_delivery_config(delivery.as_ref()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("Failed to add cron job: {e}")})),
        )
            .into_response();
    }

    // Determine job type: explicit field, or infer "agent" when prompt is provided.
    let is_agent =
        matches!(job_type.as_deref(), Some("agent")) || (job_type.is_none() && prompt.is_some());

    let result = if is_agent {
        let prompt = match prompt.as_deref() {
            Some(p) if !p.trim().is_empty() => p,
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "Missing 'prompt' for agent job"})),
                )
                    .into_response();
            }
        };

        let session_target = session_target
            .as_deref()
            .map(zeroclaw_runtime::cron::SessionTarget::parse)
            .unwrap_or_default();

        let default_delete = matches!(schedule, zeroclaw_runtime::cron::Schedule::At { .. });
        let delete_after_run = delete_after_run.unwrap_or(default_delete);

        zeroclaw_runtime::cron::add_agent_job(
            &config,
            &agent_alias,
            name,
            schedule,
            prompt,
            session_target,
            model,
            delivery,
            delete_after_run,
            allowed_tools,
            uses_memory.unwrap_or(true),
        )
    } else {
        let command = match command.as_deref() {
            Some(c) if !c.trim().is_empty() => c,
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "Missing 'command' for shell job"})),
                )
                    .into_response();
            }
        };

        zeroclaw_runtime::cron::add_shell_job_with_approval(
            &config,
            &agent_alias,
            name,
            schedule,
            command,
            delivery,
            false,
        )
    };

    match result {
        Ok(job) => Json(serde_json::json!({"status": "ok", "job": job})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to add cron job: {e}")})),
        )
            .into_response(),
    }
}

/// GET /api/cron/:id/runs — list recent runs for a cron job
pub async fn handle_api_cron_runs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(params): Query<CronRunsQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let limit = params.limit.unwrap_or(20).clamp(1, 100) as usize;
    let config = state.config.read().clone();

    // Verify the job exists before listing runs.
    if let Err(e) = zeroclaw_runtime::cron::get_job(&config, &id) {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("Cron job not found: {e}")})),
        )
            .into_response();
    }

    match zeroclaw_runtime::cron::list_runs(&config, &id, limit) {
        Ok(runs) => {
            let runs_json: Vec<serde_json::Value> = runs
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "id": r.id,
                        "job_id": r.job_id,
                        "started_at": r.started_at.to_rfc3339(),
                        "finished_at": r.finished_at.to_rfc3339(),
                        "status": r.status,
                        "output": r.output,
                        "duration_ms": r.duration_ms,
                    })
                })
                .collect();
            Json(serde_json::json!({"runs": runs_json})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to list cron runs: {e}")})),
        )
            .into_response(),
    }
}

/// POST /api/cron/:id/run — trigger a cron job manually
pub async fn handle_api_cron_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.read().clone();

    let job = match zeroclaw_runtime::cron::get_job(&config, &id) {
        Ok(job) => job,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": format!("Cron job not found: {e}")})),
            )
                .into_response();
        }
    };

    let event_tx = Some(state.event_tx.clone());
    let result = zeroclaw_runtime::cron::scheduler::run_manual_job(
        &config,
        &job,
        zeroclaw_runtime::cron::scheduler::CronDeliveryContext::GatewayManual,
        &event_tx,
    )
    .await;

    Json(serde_json::json!({
        "status": result.status,
        "job_id": result.job_id,
        "success": result.success,
        "output": result.output,
        "duration_ms": result.duration_ms,
        "started_at": result.started_at.to_rfc3339(),
        "finished_at": result.finished_at.to_rfc3339(),
    }))
    .into_response()
}

/// PATCH /api/cron/:id — update an existing cron job
pub async fn handle_api_cron_patch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<CronPatchBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.read().clone();
    let agent_alias = body.agent.clone();
    let CronPatchBody {
        agent: _,
        name,
        schedule: schedule_expr,
        tz,
        clear_tz,
        command,
        prompt,
        enabled,
        uses_memory,
    } = body;
    let timezone_patch = match parse_timezone_patch(tz, clear_tz) {
        Ok(patch) => patch,
        Err(e) => return e.into_response(),
    };

    let existing = match zeroclaw_runtime::cron::get_job(&config, &id) {
        Ok(j) => j,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": format!("Cron job not found: {e}")})),
            )
                .into_response();
        }
    };
    let is_agent = matches!(existing.job_type, zeroclaw_runtime::cron::JobType::Agent);
    let setting_shell_command = !is_agent && (command.is_some() || prompt.is_some());
    if setting_shell_command && config.agent(&agent_alias).is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!(
                "Unknown agent {a:?} (no [agents.{a}] entry configured)",
                a = agent_alias
            )})),
        )
            .into_response();
    }
    let new_expr = schedule_expr
        .as_deref()
        .map(str::trim)
        .filter(|expr| !expr.is_empty())
        .map(str::to_string);
    let timezone_changed = !matches!(timezone_patch, CronTimezonePatch::Preserve);
    let schedule = if new_expr.is_some() || timezone_changed {
        let (expr, existing_tz) = match (&existing.schedule, new_expr) {
            (_, Some(expr)) => {
                let existing_tz = match &existing.schedule {
                    zeroclaw_runtime::cron::Schedule::Cron { tz, .. } => tz.clone(),
                    _ => None,
                };
                (expr, existing_tz)
            }
            (zeroclaw_runtime::cron::Schedule::Cron { expr, tz }, None) => {
                (expr.clone(), tz.clone())
            }
            (_, None) => {
                return bad_request("tz can only be updated on cron schedules").into_response();
            }
        };
        let tz = match timezone_patch {
            CronTimezonePatch::Preserve => existing_tz,
            CronTimezonePatch::Set(tz) => Some(tz),
            CronTimezonePatch::Clear => None,
        };
        match cron_schedule_from_api(expr, tz) {
            Ok(schedule) => Some(schedule),
            Err(e) => return e.into_response(),
        }
    } else {
        None
    };
    let (patch_command, patch_prompt) = if is_agent {
        (None, command.or(prompt))
    } else {
        (command.or(prompt), None)
    };

    let patch = zeroclaw_runtime::cron::CronJobPatch {
        name,
        schedule,
        command: patch_command,
        prompt: patch_prompt,
        enabled,
        uses_memory,
        ..zeroclaw_runtime::cron::CronJobPatch::default()
    };

    match zeroclaw_runtime::cron::update_shell_job_with_approval(
        &config,
        &agent_alias,
        &id,
        patch,
        false,
    ) {
        Ok(job) => Json(serde_json::json!({"status": "ok", "job": job})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to update cron job: {e}")})),
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

    let config = state.config.read().clone();
    match zeroclaw_runtime::cron::remove_job(&config, &id) {
        Ok(()) => Json(serde_json::json!({"status": "ok"})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to remove cron job: {e}")})),
        )
            .into_response(),
    }
}

/// GET /api/cron/settings — return cron subsystem settings
pub async fn handle_api_cron_settings_get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.read().clone();
    Json(serde_json::json!({
        "enabled": config.scheduler.enabled,
        "catch_up_on_startup": config.scheduler.catch_up_on_startup,
        "max_run_history": config.scheduler.max_run_history,
    }))
    .into_response()
}

/// PATCH /api/cron/settings — update cron subsystem settings
pub async fn handle_api_cron_settings_patch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let mut config = state.config.read().clone();

    if let Some(v) = body.get("enabled").and_then(|v| v.as_bool()) {
        config.scheduler.enabled = v;
        config.mark_dirty("scheduler.enabled");
    }
    if let Some(v) = body.get("catch_up_on_startup").and_then(|v| v.as_bool()) {
        config.scheduler.catch_up_on_startup = v;
        config.mark_dirty("scheduler.catch-up-on-startup");
    }
    if let Some(v) = body.get("max_run_history").and_then(|v| v.as_u64()) {
        config.scheduler.max_run_history = u32::try_from(v).unwrap_or(u32::MAX);
        config.mark_dirty("scheduler.max-run-history");
    }

    if let Err(e) = config.save_dirty().await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to save config: {e}")})),
        )
            .into_response();
    }

    *state.config.write() = config.clone();

    Json(serde_json::json!({
        "status": "ok",
        "enabled": config.scheduler.enabled,
        "catch_up_on_startup": config.scheduler.catch_up_on_startup,
        "max_run_history": config.scheduler.max_run_history,
    }))
    .into_response()
}

/// GET /api/integrations — list all integrations with status
pub async fn handle_api_integrations(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.read().clone();
    let entries = zeroclaw_runtime::integrations::registry::all_integrations(&config);

    let integrations: Vec<serde_json::Value> = entries.iter().map(integration_entry_json).collect();

    Json(serde_json::json!({"integrations": integrations})).into_response()
}

/// GET /api/integrations/settings — return per-integration settings (enabled + category)
pub async fn handle_api_integrations_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.read().clone();
    let entries = zeroclaw_runtime::integrations::registry::all_integrations(&config);

    let mut settings = serde_json::Map::new();
    for entry in &entries {
        let enabled = matches!(
            entry.status,
            zeroclaw_runtime::integrations::IntegrationStatus::Active
        );
        settings.insert(
            entry.name.clone(),
            serde_json::json!({
                "enabled": enabled,
                "category": entry.category,
                "status": entry.status,
            }),
        );
    }

    Json(serde_json::json!({"settings": settings})).into_response()
}

/// POST /api/doctor — run diagnostics
pub async fn handle_api_doctor(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.read().clone();
    let results = zeroclaw_runtime::doctor::diagnose(&config);

    let ok_count = results
        .iter()
        .filter(|r| r.severity == zeroclaw_runtime::doctor::Severity::Ok)
        .count();
    let warn_count = results
        .iter()
        .filter(|r| r.severity == zeroclaw_runtime::doctor::Severity::Warn)
        .count();
    let error_count = results
        .iter()
        .filter(|r| r.severity == zeroclaw_runtime::doctor::Severity::Error)
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

async fn resolve_memory_handle(
    state: &AppState,
    agent_alias: Option<&str>,
) -> Result<std::sync::Arc<dyn zeroclaw_memory::Memory>, (StatusCode, Json<serde_json::Value>)> {
    let alias = match agent_alias.map(str::trim).filter(|s| !s.is_empty()) {
        Some(a) => a,
        None => return Ok(state.mem.clone()),
    };
    let config = state.config.read().clone();
    if config.agent(alias).is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!(
                "Unknown agent {alias:?} (no [agents.{alias}] entry configured)"
            )})),
        ));
    }
    let api_key = config
        .resolved_model_provider_for_agent(alias)
        .and_then(|(_, _, cfg)| cfg.api_key.clone());
    zeroclaw_memory::create_memory_for_agent(&config, alias, api_key.as_deref())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    serde_json::json!({"error": format!("Failed to build per-agent memory: {e:#}")}),
                ),
            )
        })
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

    let mem = match resolve_memory_handle(&state, params.agent.as_deref()).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };

    // Use recall when query or time range is provided
    if params.query.is_some() || params.since.is_some() || params.until.is_some() {
        let query = params.query.as_deref().unwrap_or("");
        let since = params.since.as_deref();
        let until = params.until.as_deref();
        match mem.recall(query, 50, None, since, until).await {
            Ok(entries) => {
                let entries = match params.category.as_deref() {
                    Some(cat) => entries
                        .into_iter()
                        .filter(|e| e.category.to_string() == cat)
                        .collect(),
                    None => entries,
                };
                Json(serde_json::json!({
                    "entries": sanitize_memory_entries_for_api(entries)
                }))
                .into_response()
            }
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Memory recall failed: {e}")})),
            )
                .into_response(),
        }
    } else {
        // List mode
        let category = params.category.as_deref().map(|cat| match cat {
            "core" => zeroclaw_memory::MemoryCategory::Core,
            "daily" => zeroclaw_memory::MemoryCategory::Daily,
            "conversation" => zeroclaw_memory::MemoryCategory::Conversation,
            other => zeroclaw_memory::MemoryCategory::Custom(other.to_string()),
        });

        match mem.list(category.as_ref(), None).await {
            Ok(entries) => Json(serde_json::json!({
                "entries": sanitize_memory_entries_for_api(entries)
            }))
            .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Memory list failed: {e}")})),
            )
                .into_response(),
        }
    }
}

fn sanitize_memory_entries_for_api(entries: Vec<MemoryEntry>) -> Vec<MemoryEntry> {
    entries
        .into_iter()
        .map(|mut entry| {
            entry.content = truncate_with_ellipsis_total_chars(entry.content);
            entry
        })
        .collect()
}

fn truncate_with_ellipsis_total_chars(mut s: String) -> String {
    if s.char_indices().nth(MEMORY_API_CONTENT_MAX_CHARS).is_none() {
        return s;
    }

    let keep_chars = MEMORY_API_CONTENT_MAX_CHARS - 3;
    let cut_idx = s
        .char_indices()
        .nth(keep_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(s.len());
    s.truncate(cut_idx);
    s.push_str("...");
    s
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
            "core" => zeroclaw_memory::MemoryCategory::Core,
            "daily" => zeroclaw_memory::MemoryCategory::Daily,
            "conversation" => zeroclaw_memory::MemoryCategory::Conversation,
            other => zeroclaw_memory::MemoryCategory::Custom(other.to_string()),
        })
        .unwrap_or(zeroclaw_memory::MemoryCategory::Core);

    let mem = match resolve_memory_handle(&state, body.agent.as_deref()).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };

    match mem.store(&body.key, &body.content, category, None).await {
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
    Query(query): Query<MemoryDeleteQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let mem = match resolve_memory_handle(&state, query.agent.as_deref()).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };

    match mem.forget(&key).await {
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

/// Query parameters for `GET /api/cost`. When `agent` is set, the
/// returned summary filters to records attributed to that alias.
#[derive(Debug, Deserialize)]
pub struct CostQuery {
    #[serde(default)]
    pub agent: Option<String>,
    /// RFC3339 UTC instants — caller-computed window bounds. The
    /// dashboard derives them in the operator's local timezone so
    /// "today" means the operator's today, not the daemon's UTC today.
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
}

/// GET /api/cost — cost summary over `[from, to)` (either bound omitted
/// = unbounded on that side). Pass `?agent=<alias>` for the per-agent
/// view, which ignores from/to and returns the alias's session+daily
/// rollup.
pub async fn handle_api_cost(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CostQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let parse_bound = |s: &str| {
        chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| d.with_timezone(&chrono::Utc))
    };
    let from = query.from.as_deref().and_then(parse_bound);
    let to = query.to.as_deref().and_then(parse_bound);

    if let Some(ref tracker) = state.cost_tracker {
        let result = match query.agent.as_deref().filter(|s| !s.is_empty()) {
            Some(alias) => tracker.get_summary_for_agent(alias),
            None => tracker.get_summary_in_bounds(from, to),
        };
        match result {
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
                "by_agent": {},
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

    // `discover_cli_tools` spawns child processes and blocks; keep it off the
    // async executor so a slow PATH scan can't stall other gateway requests.
    let tools = match tokio::task::spawn_blocking(|| {
        zeroclaw_tools::cli_discovery::discover_cli_tools(&[], &[])
    })
    .await
    {
        Ok(tools) => tools,
        Err(e) => {
            // The blocking task panicked; degrade to an empty list rather
            // than failing the request, but record why it was empty.
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "cli-tools discovery task failed; returning empty list"
            );
            Vec::new()
        }
    };

    Json(serde_json::json!({"cli_tools": tools})).into_response()
}

/// GET /api/channels — list configured channels with status
pub async fn handle_api_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.read().clone();
    let health = zeroclaw_runtime::health::snapshot();
    // One entry per `[channels.<type>.<alias>]` block. Owning
    // agent comes from the agents.<alias>.channels reverse lookup.
    let channels: Vec<serde_json::Value> = config
        .channels_by_alias()
        .into_iter()
        .map(|info| {
            let composite = format!("{}.{}", info.channel_type, info.alias);
            let compiled_key = compiled_readiness_key_for_alias(&config, &info);
            let compiled = zeroclaw_channels::listing::is_channel_type_compiled(compiled_key);
            let readiness = channel_readiness(&config, &info, &health, &state);
            let (status, health_status) = if compiled {
                channel_readiness_summary(&readiness)
            } else {
                ("not_compiled", "unavailable")
            };
            serde_json::json!({
                "name": composite,
                "type": info.channel_type,
                "alias": info.alias,
                "owning_agent": info.owning_agent,
                "enabled": info.enabled,
                "compiled": compiled,
                "status": status,
                "message_count": 0,
                "last_message_at": null,
                "health": health_status,
                "readiness": readiness,
            })
        })
        .collect();

    Json(serde_json::json!({ "channels": channels })).into_response()
}

/// POST /api/channels/{channel}/relink — replace a QR channel's pairing.
///
/// `{channel}` is the composite `<type>.<alias>` name returned by
/// `GET /api/channels`. Dispatches to the channel-owned relink hook
/// ([`zeroclaw_channels::login_relink::relink`]); the gateway performs no
/// file operations of its own and holds no knowledge of channel session
/// layouts.
///
/// Responses (all authenticated via the standard bearer guard):
///
/// - `200` with `"outcome": "cleared"` — persisted login removed
///   (`"removed"` lists the paths). `"restart_required": true`: the running
///   channel keeps its in-memory session until the daemon restarts it, so
///   the caller follows up with `POST /admin/reload` (which enforces its
///   own, stricter admin policy — relink deliberately does not bypass it).
/// - `200` with `"outcome": "nothing_to_clear"` — the channel supports
///   relinking but held no persisted login; the next start already mints a
///   fresh QR.
/// - `409` with `"outcome": "unsupported"` — the channel type has no relink
///   hook (it does not use QR-pairing sessions) or its feature is not
///   compiled into this binary. **Explicit no-op: nothing was touched.**
/// - `404` — no `[channels.<type>.<alias>]` block matches `{channel}`.
pub async fn handle_api_channel_relink(
    State(state): State<AppState>,
    Path(channel): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.read().clone();
    let Some(info) = config
        .channels_by_alias()
        .into_iter()
        .find(|info| format!("{}.{}", info.channel_type, info.alias) == channel)
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("unknown channel {channel} — use the composite name from GET /api/channels"),
            })),
        )
            .into_response();
    };

    // Resolve the string key to the typed QR-pairing channel once; probe
    // and relink dispatch on the same enum. `None` means the channel type
    // has no relink hook or its feature is not compiled — an explicit
    // no-op conflict where nothing is touched.
    let compiled_key = compiled_readiness_key_for_alias(&config, &info);
    let Some(qr_channel) = zeroclaw_channels::listing::qr_pairing_channel(compiled_key) else {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "channel": channel,
                "outcome": "unsupported",
                "error": format!(
                    "channel type {} has no relink operation (it does not use QR-pairing sessions) \
                     or the feature is not compiled into this binary; nothing was changed",
                    info.channel_type
                ),
            })),
        )
            .into_response();
    };

    match zeroclaw_channels::login_relink::relink(qr_channel, &config, &info.alias) {
        Ok(zeroclaw_channels::login_relink::RelinkOutcome::Cleared { removed }) => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"channel": channel, "removed": removed})),
                "channel persisted login cleared for relink"
            );
            Json(serde_json::json!({
                "channel": channel,
                "outcome": "cleared",
                "removed": removed,
                "restart_required": true,
                "note": "restart the channel (POST /admin/reload) to begin the fresh QR pairing",
            }))
            .into_response()
        }
        Ok(zeroclaw_channels::login_relink::RelinkOutcome::NothingToClear) => {
            Json(serde_json::json!({
                "channel": channel,
                "outcome": "nothing_to_clear",
                "removed": [],
                "restart_required": false,
                "note": "no persisted login was stored; the next channel start already begins a fresh QR pairing",
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "channel": channel,
                "error": format!("failed to clear persisted login: {e}"),
            })),
        )
            .into_response(),
    }
}

/// GET /api/tuis — list connected TUI sessions
pub async fn handle_api_tuis(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let tuis: Vec<serde_json::Value> = state
        .tui_registry
        .as_ref()
        .map(|r| {
            r.list()
                .into_iter()
                .map(|e| {
                    serde_json::json!({
                        "tui_id": e.tui_id,
                        "connected_at": e.connected_at.to_rfc3339(),
                        "peer_label": e.peer_label,
                        "transport": e.transport,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Json(serde_json::json!({ "tuis": tuis })).into_response()
}

fn compiled_readiness_key_for_alias<'a>(config: &'a Config, info: &'a ChannelAliasInfo) -> &'a str {
    if info.channel_type == "whatsapp"
        && config
            .channels
            .whatsapp
            .get(&info.alias)
            .is_some_and(|whatsapp| whatsapp.backend_type() == "web")
    {
        "whatsapp-web"
    } else {
        info.channel_type.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ChannelReadinessState {
    Ready,
    Missing,
    Unknown,
}

const CHANNEL_LISTENER_HEALTH_MAX_AGE_SECS: i64 = 30;

#[derive(Debug, Clone, Serialize)]
struct ChannelReadiness {
    enabled: ChannelReadinessState,
    bound_to_agent: ChannelReadinessState,
    authenticated: ChannelReadinessState,
    listening: ChannelReadinessState,
    requirements: Vec<String>,
    notes: Vec<String>,
}

fn channel_readiness(
    config: &zeroclaw_config::schema::Config,
    info: &zeroclaw_config::schema::ChannelAliasInfo,
    health: &zeroclaw_runtime::health::HealthSnapshot,
    state: &AppState,
) -> ChannelReadiness {
    let mut readiness = ChannelReadiness {
        enabled: if info.enabled {
            ChannelReadinessState::Ready
        } else {
            ChannelReadinessState::Missing
        },
        bound_to_agent: if info.owning_agent.is_some() {
            ChannelReadinessState::Ready
        } else {
            ChannelReadinessState::Missing
        },
        authenticated: ChannelReadinessState::Unknown,
        listening: ChannelReadinessState::Unknown,
        requirements: Vec::new(),
        notes: Vec::new(),
    };

    if readiness.enabled == ChannelReadinessState::Missing {
        readiness
            .requirements
            .push("Enable this channel alias.".to_string());
    }
    if readiness.bound_to_agent == ChannelReadinessState::Missing {
        readiness
            .requirements
            .push("Bind this channel to an enabled agent.".to_string());
    }

    if readiness.enabled == ChannelReadinessState::Ready
        && readiness.bound_to_agent == ChannelReadinessState::Ready
    {
        if info.channel_type == "webhook" {
            apply_webhook_readiness(config, &info.alias, health, state, &mut readiness);
        } else {
            apply_persisted_login_readiness(config, info, &mut readiness);
        }
    }

    readiness
}

/// Fill `readiness.authenticated` from the channel-owned persisted-login
/// probe (`zeroclaw_channels::login_probe`). The probe resolves the same
/// on-disk session signal each QR-pairing channel uses at startup to decide
/// between resuming a session and minting a fresh QR code; nothing is
/// cached and nothing is written. Channel types without a typed QR-pairing
/// key (no probe, or feature not compiled) keep `authenticated: unknown`
/// and the existing "not checked yet" note.
fn apply_persisted_login_readiness(
    config: &zeroclaw_config::schema::Config,
    info: &zeroclaw_config::schema::ChannelAliasInfo,
    readiness: &mut ChannelReadiness,
) {
    use zeroclaw_channels::login_probe::PersistedLogin;

    // Resolve the string key to the typed QR-pairing channel once; all
    // downstream dispatch is on the enum.
    let compiled_key = compiled_readiness_key_for_alias(config, info);
    let Some(channel) = zeroclaw_channels::listing::qr_pairing_channel(compiled_key) else {
        readiness.notes.push(format!(
            "Live readiness is not checked for `{}` channels yet.",
            info.channel_type
        ));
        return;
    };

    match zeroclaw_channels::login_probe::persisted_login(channel, config, &info.alias) {
        PersistedLogin::Present => {
            readiness.authenticated = ChannelReadinessState::Ready;
            readiness.notes.push(format!(
                "Live listener readiness is not checked for `{}` channels yet.",
                info.channel_type
            ));
        }
        PersistedLogin::Absent => {
            readiness.authenticated = ChannelReadinessState::Missing;
            readiness.requirements.push(
                "Pair this channel: no persisted login session was found on disk.".to_string(),
            );
        }
    }
}

fn channel_readiness_summary(readiness: &ChannelReadiness) -> (&'static str, &'static str) {
    if readiness.enabled == ChannelReadinessState::Missing
        || readiness.bound_to_agent == ChannelReadinessState::Missing
    {
        return ("inactive", "degraded");
    }

    if readiness.authenticated == ChannelReadinessState::Missing
        || readiness.listening == ChannelReadinessState::Missing
    {
        return ("error", "down");
    }

    if readiness.authenticated == ChannelReadinessState::Ready
        && readiness.listening == ChannelReadinessState::Ready
    {
        ("active", "healthy")
    } else {
        // At least one probe is Unknown and none reported Missing: not
        // enough signal to call the channel either healthy or down.
        ("unknown", "degraded")
    }
}

fn apply_webhook_readiness(
    config: &zeroclaw_config::schema::Config,
    alias: &str,
    health: &zeroclaw_runtime::health::HealthSnapshot,
    state: &AppState,
    readiness: &mut ChannelReadiness,
) {
    let Some(webhook) = config.channels.webhook.get(alias) else {
        readiness.authenticated = ChannelReadinessState::Missing;
        readiness.listening = ChannelReadinessState::Missing;
        readiness
            .requirements
            .push("Webhook config block is missing.".to_string());
        return;
    };

    if state.pairing.require_pairing() && !state.pairing.is_paired() {
        readiness.authenticated = ChannelReadinessState::Missing;
        readiness
            .requirements
            .push("Pair the gateway before using the webhook endpoint.".to_string());
    } else {
        readiness.authenticated = ChannelReadinessState::Ready;
    }

    let component = format!("channel:webhook.{alias}");
    let component_health = health.components.get(&component);
    let component_status = component_health.map(|component| component.status.as_str());
    let supervised_listener_ok = component_health.is_some_and(component_health_ok_and_fresh);
    let listen_path = normalized_webhook_path(webhook.listen_path.as_deref());

    if supervised_listener_ok {
        readiness.listening = ChannelReadinessState::Ready;
    } else if component_status == Some("error") {
        readiness.listening = ChannelReadinessState::Missing;
        readiness.requirements.push(format!(
            "Resolve the listener error for `webhook.{alias}` before using this channel."
        ));
    } else {
        readiness.listening = ChannelReadinessState::Missing;
        readiness.requirements.push(format!(
            "Start a channel listener for `webhook.{alias}` on port {}{}.",
            webhook.port, listen_path
        ));
    }
}

fn component_health_ok_and_fresh(component: &zeroclaw_runtime::health::ComponentHealth) -> bool {
    if component.status != "ok" {
        return false;
    }

    let Ok(updated_at) = chrono::DateTime::parse_from_rfc3339(&component.updated_at) else {
        return false;
    };
    let age = chrono::Utc::now().signed_duration_since(updated_at.with_timezone(&chrono::Utc));
    age >= chrono::Duration::zero()
        && age <= chrono::Duration::seconds(CHANNEL_LISTENER_HEALTH_MAX_AGE_SECS)
}

fn normalized_webhook_path(path: Option<&str>) -> String {
    let trimmed = path.unwrap_or("/webhook").trim();
    if trimmed.is_empty() {
        "/webhook".to_string()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
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

    let snapshot = zeroclaw_runtime::health::snapshot();
    Json(serde_json::json!({"health": snapshot})).into_response()
}

// ── Helpers ─────────────────────────────────────────────────────

/// Error returned when a session key cannot be resolved unambiguously
/// because both `gw_{id}` and `{id}` exist in the backend.
#[derive(Debug)]
struct SessionKeyResolutionError {
    id: String,
    gw_candidate: String,
    bare_candidate: String,
}

/// Resolve a session key from a caller-supplied ID by consulting the backend.
///
/// Strategy (in order):
/// 1. `gw_` prefix → already a full gateway key (identity after sanitize).
/// 2. Probe both `gw_{sanitize(id)}` and `{sanitize(id)}` bare.
///    - Both exist → `Err(SessionKeyResolutionError)` — ambiguous.
///    - Only `gw_` exists → return `gw_` form.
///    - Only bare exists → return bare form (channel key).
///    - Neither exists → default to `gw_{sanitize(id)}`.
fn resolve_session_key(
    id: &str,
    backend: &dyn zeroclaw_infra::session_backend::SessionBackend,
) -> Result<String, SessionKeyResolutionError> {
    if id.starts_with("gw_") {
        return Ok(zeroclaw_api::session_keys::sanitize_session_key(id));
    }
    let bare = zeroclaw_api::session_keys::sanitize_session_key(id);
    let gw_key = format!("gw_{}", bare);
    let gw_exists = backend.session_exists(&gw_key);
    let bare_exists = backend.session_exists(&bare);
    match (gw_exists, bare_exists) {
        (true, true) => Err(SessionKeyResolutionError {
            id: id.to_string(),
            gw_candidate: gw_key,
            bare_candidate: bare,
        }),
        (true, false) => Ok(gw_key),
        (false, true) => Ok(bare),
        (false, false) => Ok(gw_key),
    }
}

// ── Session API handlers ─────────────────────────────────────────

/// GET /api/sessions — list gateway sessions
pub async fn handle_api_sessions_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let Some(ref backend) = state.session_backend else {
        return Json(serde_json::json!({
            "sessions": [],
            "message": "Session persistence is disabled"
        }))
        .into_response();
    };

    // Include every session that's attributable (agent_alias stamped,
    // or a channel_id that resolves to an owning agent).
    // Pre-migration rows with neither set are skipped as orphans.
    let config = state.config.read().clone();
    let all_metadata = backend.list_sessions_with_metadata();
    let sessions: Vec<serde_json::Value> = all_metadata
        .into_iter()
        .filter(|meta| meta.agent_alias.is_some() || meta.channel_id.is_some())
        .map(|meta| {
            // Resolve owning agent: prefer the stamped alias, otherwise
            // reverse-look-up via channel_id (= `<type>.<alias>`) against
            // each agent's `channels` list.
            let agent_alias = meta.agent_alias.clone().or_else(|| {
                meta.channel_id
                    .as_deref()
                    .and_then(|c| config.agent_for_channel(c))
                    .map(str::to_string)
            });
            // Drop the gw_ prefix for display; channel keys stay as-is so
            // the frontend can show the channel context inline.
            let session_id = meta
                .key
                .strip_prefix("gw_")
                .map(str::to_string)
                .unwrap_or_else(|| meta.key.clone());
            let mut entry = serde_json::json!({
                // Display form: `gw_` stripped for gateway sessions, full
                // composite for channel-driven sessions.
                "session_id": session_id,
                // Full DB key for API operations (delete, messages, abort).
                "session_key": meta.key.clone(),
                "created_at": meta.created_at.to_rfc3339(),
                "last_activity": meta.last_activity.to_rfc3339(),
                "message_count": meta.message_count,
                "agent_alias": agent_alias,
                "channel_id": meta.channel_id,
            });
            if let Some(name) = meta.name {
                entry["name"] = serde_json::Value::String(name);
            }
            entry
        })
        .collect();

    Json(serde_json::json!({ "sessions": sessions })).into_response()
}

/// GET /api/sessions/{id}/messages — load persisted gateway WebSocket chat transcript
pub async fn handle_api_session_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let Some(ref backend) = state.session_backend else {
        return Json(serde_json::json!({
            "session_id": id,
            "messages": [],
            "session_persistence": false,
        }))
        .into_response();
    };

    // Accept either the full DB key (channel-driven sessions like
    // `discord.clamps_…`) or the stripped form (legacy callers that pass
    // just the UUID for gateway sessions).
    let session_key = match resolve_session_key(&id, backend.as_ref()) {
        Ok(key) => key,
        Err(e) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "ambiguous_session_key",
                    "error_description": format!("The session identifier '{}' resolves to multiple sessions. Use the full storage key (with 'gw_' prefix) to disambiguate.", e.id),
                    "candidates": [e.gw_candidate, e.bare_candidate],
                    "hint": "Use the 'session_key' field from GET /api/sessions responses instead of 'session_id'."
                })),
            ).into_response();
        }
    };
    let msgs = backend.load_with_timestamps(&session_key);
    let messages: Vec<serde_json::Value> = msgs
        .into_iter()
        .map(|m| {
            serde_json::json!({
                "role": m.message.role,
                "content": m.message.content,
                "created_at": m.created_at.map(|dt| dt.to_rfc3339()),
            })
        })
        .collect();

    Json(serde_json::json!({
        "session_id": id,
        "messages": messages,
        "session_persistence": true,
    }))
    .into_response()
}

/// POST /api/sessions/{id}/messages — push a visible notification into a gateway session
pub async fn handle_api_session_message_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SessionMessagePostBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    if body.content.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "content is required"})),
        )
            .into_response();
    }

    let Some(ref backend) = state.session_backend else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Session persistence is disabled"})),
        )
            .into_response();
    };

    let session_key = match resolve_session_key(&id, backend.as_ref()) {
        Ok(key) => key,
        Err(e) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "ambiguous_session_key",
                    "error_description": format!("The session identifier '{}' resolves to multiple sessions. Use the full storage key (with 'gw_' prefix) to disambiguate.", e.id),
                    "candidates": [e.gw_candidate, e.bare_candidate],
                    "hint": "Use the 'session_key' field from GET /api/sessions responses instead of 'session_id'."
                })),
            ).into_response();
        }
    };
    if !backend
        .list_sessions()
        .iter()
        .any(|key| key == &session_key)
    {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Session not found"})),
        )
            .into_response();
    }

    let _session_guard = match state.session_queue.acquire(&session_key).await {
        Ok(guard) => guard,
        Err(crate::session_queue::SessionQueueError::QueueFull { .. }) => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({"error": "Session queue is full"})),
            )
                .into_response();
        }
        Err(crate::session_queue::SessionQueueError::Timeout { .. }) => {
            return (
                StatusCode::REQUEST_TIMEOUT,
                Json(serde_json::json!({"error": "Timed out waiting for session queue"})),
            )
                .into_response();
        }
    };

    let message = zeroclaw_providers::ChatMessage::assistant(&body.content);
    if let Err(e) = backend.append(&session_key, &message) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to append session message: {e}")})),
        )
            .into_response();
    }

    // Use the raw dashboard session ID here to match the WS `?session_id=`
    // query parameter; the `gw_` storage key is only for persistence.
    let event = serde_json::json!({
        "type": "message",
        "session_id": id.clone(),
        "role": "assistant",
        "content": body.content.clone(),
        "source": "api",
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });
    let _ = state.event_tx.send(event);

    Json(serde_json::json!({
        "status": "ok",
        "session_id": id,
        "message": {
            "role": "assistant",
            "content": message.content,
        },
        "session_persistence": true,
    }))
    .into_response()
}

/// DELETE /api/sessions/{id} — delete a gateway session
pub async fn handle_api_session_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let Some(ref backend) = state.session_backend else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Session persistence is disabled"})),
        )
            .into_response();
    };

    let session_key = match resolve_session_key(&id, backend.as_ref()) {
        Ok(key) => key,
        Err(e) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "ambiguous_session_key",
                    "error_description": format!("The session identifier '{}' resolves to multiple sessions. Use the full storage key (with 'gw_' prefix) to disambiguate.", e.id),
                    "candidates": [e.gw_candidate, e.bare_candidate],
                    "hint": "Use the 'session_key' field from GET /api/sessions responses instead of 'session_id'."
                })),
            ).into_response();
        }
    };

    let token = state
        .cancel_tokens
        .lock()
        .expect("cancel_tokens lock poisoned")
        .remove(&session_key);
    if let Some(token) = token {
        token.cancel();
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"session_key": session_key})),
            "cancelled in-flight turn for deleted session"
        );
    }

    // Hold the session queue so any in-flight turn finishes persistence
    // before the session is deleted, preventing resurrection.
    let _session_guard = match state.session_queue.acquire(&session_key).await {
        Ok(guard) => guard,
        Err(crate::session_queue::SessionQueueError::QueueFull { .. }) => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({"error": "Session queue is full"})),
            )
                .into_response();
        }
        Err(crate::session_queue::SessionQueueError::Timeout { .. }) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "Session is busy, retry after current turn completes"
                })),
            )
                .into_response();
        }
    };

    match backend.delete_session(&session_key) {
        Ok(true) => Json(serde_json::json!({"deleted": true, "session_id": id})).into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Session not found"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to delete session: {e}")})),
        )
            .into_response(),
    }
}

/// PUT /api/sessions/{id} — rename a gateway session
pub async fn handle_api_session_rename(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let Some(ref backend) = state.session_backend else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Session persistence is disabled"})),
        )
            .into_response();
    };

    let name = body["name"].as_str().unwrap_or("").trim();
    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "name is required"})),
        )
            .into_response();
    }

    let session_key = match resolve_session_key(&id, backend.as_ref()) {
        Ok(key) => key,
        Err(e) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "ambiguous_session_key",
                    "error_description": format!("The session identifier '{}' resolves to multiple sessions. Use the full storage key (with 'gw_' prefix) to disambiguate.", e.id),
                    "candidates": [e.gw_candidate, e.bare_candidate],
                    "hint": "Use the 'session_key' field from GET /api/sessions responses instead of 'session_id'."
                })),
            ).into_response();
        }
    };

    // Verify the session exists before renaming
    let sessions = backend.list_sessions();
    if !sessions.contains(&session_key) {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Session not found"})),
        )
            .into_response();
    }

    match backend.set_session_name(&session_key, name) {
        Ok(()) => Json(serde_json::json!({"session_id": id, "name": name})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to rename session: {e}")})),
        )
            .into_response(),
    }
}

/// GET /api/sessions/running — list sessions currently in "running" state
pub async fn handle_api_sessions_running(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let Some(ref backend) = state.session_backend else {
        return Json(serde_json::json!({
            "sessions": [],
            "message": "Session persistence is disabled"
        }))
        .into_response();
    };

    let running = backend.list_running_sessions();
    let sessions: Vec<serde_json::Value> = running
        .into_iter()
        .filter_map(|meta| {
            let session_id = meta.key.strip_prefix("gw_")?;
            Some(serde_json::json!({
                "session_id": session_id,
                "created_at": meta.created_at.to_rfc3339(),
                "last_activity": meta.last_activity.to_rfc3339(),
                "message_count": meta.message_count,
            }))
        })
        .collect();

    Json(serde_json::json!({ "sessions": sessions })).into_response()
}

/// GET /api/sessions/{id}/state — get session state
pub async fn handle_api_session_state(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let Some(ref backend) = state.session_backend else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Session persistence is disabled"})),
        )
            .into_response();
    };

    let session_key = match resolve_session_key(&id, backend.as_ref()) {
        Ok(key) => key,
        Err(e) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "ambiguous_session_key",
                    "error_description": format!("The session identifier '{}' resolves to multiple sessions. Use the full storage key (with 'gw_' prefix) to disambiguate.", e.id),
                    "candidates": [e.gw_candidate, e.bare_candidate],
                    "hint": "Use the 'session_key' field from GET /api/sessions responses instead of 'session_id'."
                })),
            ).into_response();
        }
    };
    match backend.get_session_state(&session_key) {
        Ok(Some(ss)) => {
            let mut resp = serde_json::json!({
                "session_id": id,
                "state": ss.state,
            });
            if let Some(turn_id) = ss.turn_id {
                resp["turn_id"] = serde_json::Value::String(turn_id);
            }
            if let Some(started) = ss.turn_started_at {
                resp["turn_started_at"] = serde_json::Value::String(started.to_rfc3339());
            }
            Json(resp).into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Session not found"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to get session state: {e}")})),
        )
            .into_response(),
    }
}

// ── Session abort endpoint ────────────────────────────────────────

pub async fn handle_api_session_abort(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let session_key = match state.session_backend.as_ref() {
        Some(backend) => match resolve_session_key(&id, backend.as_ref()) {
            Ok(key) => key,
            Err(e) => {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": "ambiguous_session_key",
                        "error_description": format!("The session identifier '{}' resolves to multiple sessions. Use the full storage key (with 'gw_' prefix) to disambiguate.", e.id),
                        "candidates": [e.gw_candidate, e.bare_candidate],
                        "hint": "Use the 'session_key' field from GET /api/sessions responses instead of 'session_id'."
                    })),
                ).into_response();
            }
        },
        None => format!(
            "gw_{}",
            zeroclaw_api::session_keys::sanitize_session_key(&id)
        ),
    };

    // Look up and cancel the token. Hold the lock only long enough to
    // clone the token — cancellation itself does not need the lock.
    let token = state
        .cancel_tokens
        .lock()
        .expect("cancel_tokens lock poisoned")
        .get(&session_key)
        .cloned();

    if let Some(token) = token {
        token.cancel();
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"session_key": session_key})),
            "session abort requested"
        );
        Json(serde_json::json!({ "status": "aborted" })).into_response()
    } else {
        Json(serde_json::json!({ "status": "no_active_response" })).into_response()
    }
}

// ── Claude Code hook endpoint ────────────────────────────────────

pub async fn handle_claude_code_hook(
    State(state): State<AppState>,
    Json(payload): Json<zeroclaw_tools::claude_code_runner::ClaudeCodeHookEvent>,
) -> impl IntoResponse {
    // Do not require bearer-token auth: Claude Code subprocesses cannot easily
    // obtain a pairing token, and the hook carries a session_id that ties it
    // back to a session we spawned.
    let _ = &state; // retained for future Slack update wiring

    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"session_id": payload.session_id, "event_type": payload.event_type, "tool_name": payload.tool_name, "summary": payload.summary})), "Claude Code hook event received");

    Json(serde_json::json!({ "ok": true }))
}

// Shared test helper: `api_config` tests reuse this AppState builder for the
// agent rename/delete cascade handlers/coverage).

#[cfg(test)]
pub(crate) use tests::test_state;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{AppState, GatewayRateLimiter, IdempotencyStore, nodes};
    use async_trait::async_trait;
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;
    use parking_lot::RwLock;
    #[cfg(feature = "channel-linq")]
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;
    use zeroclaw_infra::session_backend::SessionBackend;
    use zeroclaw_infra::session_store::SessionStore;
    use zeroclaw_memory::{Memory, MemoryCategory, MemoryEntry};
    use zeroclaw_providers::{ChatMessage, ModelProvider};
    use zeroclaw_runtime::security::pairing::PairingGuard;

    #[derive(Default)]
    struct MockMemory {
        entries: Vec<MemoryEntry>,
    }

    #[async_trait]
    impl Memory for MockMemory {
        fn name(&self) -> &str {
            "mock"
        }

        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(self.entries.clone())
        }

        async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
            Ok(None)
        }

        async fn list(
            &self,
            _category: Option<&MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(self.entries.clone())
        }

        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(self.entries.len())
        }

        async fn health_check(&self) -> bool {
            true
        }

        async fn store_with_agent(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
            _namespace: Option<&str>,
            _importance: Option<f64>,
            _agent_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall_for_agents(
            &self,
            _allowed_agent_ids: &[&str],
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn purge_agent(&self, _agent_alias: &str) -> anyhow::Result<usize> {
            Ok(0)
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for MockMemory {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Memory(
                ::zeroclaw_api::attribution::MemoryKind::InMemory,
            )
        }
        fn alias(&self) -> &str {
            "MockMemory"
        }
    }

    struct MockModelProvider;

    #[async_trait]
    impl ModelProvider for MockModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for MockModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "MockModelProvider"
        }
    }

    /// Wire a minimal agent + model_provider + risk_profile into a test config
    /// so cron-add API tests have an `agent` reference to bind to.
    fn with_test_agent(
        mut config: zeroclaw_config::schema::Config,
    ) -> zeroclaw_config::schema::Config {
        config.providers.models.openrouter.insert(
            "default".to_string(),
            zeroclaw_config::schema::OpenRouterModelProviderConfig::default(),
        );
        config.risk_profiles.insert(
            "test-profile".to_string(),
            zeroclaw_config::schema::RiskProfileConfig::default(),
        );
        config.agents.insert(
            "test-agent".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "openrouter.default".into(),
                risk_profile: "test-profile".into(),
                ..Default::default()
            },
        );
        config
    }

    pub(crate) fn test_state(config: zeroclaw_config::schema::Config) -> AppState {
        AppState {
            config: Arc::new(RwLock::new(config)),
            model_provider: Arc::new(MockModelProvider),
            model: "test-model".into(),
            temperature: None,
            mem: Arc::new(MockMemory::default()),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(MockMemory::default()),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100, 100)),
            auth_limiter: Arc::new(crate::auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp: HashMap::new(),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp_app_secret: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq_signing_secrets: HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk: HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk_webhook_secret: HashMap::new(),
            #[cfg(feature = "channel-wati")]
            wati: HashMap::new(),
            #[cfg(feature = "channel-email")]
            gmail_push: None,
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            tools_registry_by_agent: Arc::new(std::collections::HashMap::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(crate::sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            session_backend: None,
            session_queue: Arc::new(crate::session_queue::SessionActorQueue::new(8, 30, 600)),
            device_registry: None,
            pending_pairings: None,
            path_prefix: String::new(),
            web_dist_dir: None,
            canvas_store: zeroclaw_runtime::tools::CanvasStore::new(),
            cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tui_registry: None,
            reload_tx: None,
            sop_engine: None,
            sop_audit: None,
            #[cfg(feature = "webauthn")]
            webauthn: None,
        }
    }

    fn test_state_with_memory(
        config: zeroclaw_config::schema::Config,
        entries: Vec<MemoryEntry>,
    ) -> AppState {
        AppState {
            mem: Arc::new(MockMemory { entries }),
            ..test_state(config)
        }
    }

    async fn response_json(response: axum::response::Response) -> serde_json::Value {
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        serde_json::from_slice(&body).expect("valid json response")
    }

    #[test]
    fn integration_entry_json_derives_category_label_from_category() {
        let entry = zeroclaw_runtime::integrations::IntegrationEntry {
            name: "Browser".into(),
            description: "Run browser automation".into(),
            category: zeroclaw_runtime::integrations::IntegrationCategory::ToolsAutomation,
            status: zeroclaw_runtime::integrations::IntegrationStatus::Active,
        };

        let json = integration_entry_json(&entry);

        assert_eq!(json["category"], "ToolsAutomation");
        assert_eq!(json["category_label"], "Tools & Automation");
        assert_eq!(json["status"], "Active");
    }

    fn memory_entry_with_content(content: String) -> MemoryEntry {
        MemoryEntry {
            id: "entry-1".into(),
            key: "huge-memory".into(),
            content,
            category: MemoryCategory::Conversation,
            timestamp: "2026-04-06T00:00:00Z".into(),
            session_id: None,
            score: None,
            namespace: "default".into(),
            importance: Some(0.5),
            superseded_by: None,
            kind: None,
            pinned: false,
            tenant_id: None,
            agent_alias: None,
            agent_id: None,
        }
    }

    fn memory_content_from_response(json: &serde_json::Value) -> &str {
        json["entries"][0]["content"]
            .as_str()
            .expect("string content")
    }

    #[test]
    fn truncate_memory_api_content_caps_total_chars_with_ellipsis() {
        let exact = "x".repeat(MEMORY_API_CONTENT_MAX_CHARS);
        assert_eq!(truncate_with_ellipsis_total_chars(exact.clone()), exact);

        let short = "short memory".to_string();
        assert_eq!(truncate_with_ellipsis_total_chars(short.clone()), short);

        let over = "火".repeat(MEMORY_API_CONTENT_MAX_CHARS + 1);
        let truncated = truncate_with_ellipsis_total_chars(over.clone());
        assert_eq!(truncated.chars().count(), MEMORY_API_CONTENT_MAX_CHARS);
        assert!(truncated.ends_with("..."));
        assert_ne!(truncated, over);
    }

    #[tokio::test]
    async fn handle_api_memory_list_truncates_oversized_content() {
        let mut config = zeroclaw_config::schema::Config::default();
        config.gateway.require_pairing = false;
        let huge = "x".repeat(MEMORY_API_CONTENT_MAX_CHARS + 128);
        let state = test_state_with_memory(config, vec![memory_entry_with_content(huge.clone())]);

        let response = handle_api_memory_list(
            State(state),
            HeaderMap::new(),
            Query(MemoryQuery {
                query: None,
                category: None,
                since: None,
                until: None,
                agent: None,
            }),
        )
        .await
        .into_response();

        let json = response_json(response).await;
        let content = memory_content_from_response(&json);

        assert_eq!(content.chars().count(), MEMORY_API_CONTENT_MAX_CHARS);
        assert!(content.ends_with("..."));
        assert_eq!(json["entries"][0]["key"], "huge-memory");
        assert_eq!(json["entries"][0]["category"], "conversation");
        assert_ne!(content, huge);
    }

    #[tokio::test]
    async fn handle_api_memory_search_truncates_oversized_content_after_filtering() {
        let mut config = zeroclaw_config::schema::Config::default();
        config.gateway.require_pairing = false;
        let huge = "火".repeat(MEMORY_API_CONTENT_MAX_CHARS + 128);
        let state = test_state_with_memory(config, vec![memory_entry_with_content(huge.clone())]);

        let response = handle_api_memory_list(
            State(state),
            HeaderMap::new(),
            Query(MemoryQuery {
                query: Some("huge".into()),
                category: Some("conversation".into()),
                since: None,
                until: None,
                agent: None,
            }),
        )
        .await
        .into_response();

        let json = response_json(response).await;
        let content = memory_content_from_response(&json);

        assert_eq!(content.chars().count(), MEMORY_API_CONTENT_MAX_CHARS);
        assert!(content.ends_with("..."));
        assert_ne!(content, huge);
    }

    #[tokio::test]
    async fn handle_api_tools_scopes_listing_by_agent_query() {
        use zeroclaw_api::tool::ToolSpec;

        let mut config = zeroclaw_config::schema::Config::default();
        config.gateway.require_pairing = false;
        let mut state = test_state(config);

        let spec = |name: &str| {
            ToolSpec::new(
                name.to_string(),
                format!("{name} desc"),
                serde_json::json!({}),
            )
        };
        state.tools_registry = Arc::new(vec![spec("default_tool")]);
        let mut by_agent: std::collections::HashMap<String, Arc<Vec<ToolSpec>>> =
            std::collections::HashMap::new();
        by_agent.insert("alpha".to_string(), Arc::new(vec![spec("alpha_tool")]));
        by_agent.insert("beta".to_string(), Arc::new(vec![spec("beta_tool")]));
        state.tools_registry_by_agent = Arc::new(by_agent);

        async fn tool_names(state: AppState, agent: Option<&str>) -> Vec<String> {
            let response = handle_api_tools(
                State(state),
                HeaderMap::new(),
                Query(ToolsQuery {
                    agent: agent.map(str::to_string),
                }),
            )
            .await
            .into_response();
            response_json(response).await["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string())
                .collect()
        }

        // A known agent gets its own scoped listing.
        assert_eq!(
            tool_names(state.clone(), Some("beta")).await,
            vec!["beta_tool".to_string()]
        );
        // Omitted agent falls back to the default seed listing.
        assert_eq!(
            tool_names(state.clone(), None).await,
            vec!["default_tool".to_string()]
        );
        // Unknown and blank aliases fall back to the default rather than error,
        // so a stale UI selection still renders something.
        assert_eq!(
            tool_names(state.clone(), Some("ghost")).await,
            vec!["default_tool".to_string()]
        );
        assert_eq!(
            tool_names(state.clone(), Some("   ")).await,
            vec!["default_tool".to_string()]
        );
    }

    #[test]
    fn api_channels_readiness_key_tracks_whatsapp_backend_type() {
        let mut config = zeroclaw_config::schema::Config::default();
        config.channels.whatsapp.insert(
            "web".to_string(),
            zeroclaw_config::schema::WhatsAppConfig {
                enabled: true,
                session_path: Some("~/.zeroclaw/state/whatsapp-web/session.db".into()),
                ..Default::default()
            },
        );
        config.channels.whatsapp.insert(
            "cloud".to_string(),
            zeroclaw_config::schema::WhatsAppConfig {
                enabled: true,
                access_token: Some("token".into()),
                phone_number_id: Some("phone-id".into()),
                verify_token: Some("verify".into()),
                ..Default::default()
            },
        );
        config.channels.whatsapp.insert(
            "ambiguous".to_string(),
            zeroclaw_config::schema::WhatsAppConfig {
                enabled: true,
                access_token: Some("token".into()),
                phone_number_id: Some("phone-id".into()),
                verify_token: Some("verify".into()),
                session_path: Some("~/.zeroclaw/state/whatsapp-web/session.db".into()),
                ..Default::default()
            },
        );

        let web = zeroclaw_config::schema::ChannelAliasInfo {
            channel_type: "whatsapp".to_string(),
            alias: "web".to_string(),
            owning_agent: None,
            enabled: true,
        };
        let cloud = zeroclaw_config::schema::ChannelAliasInfo {
            channel_type: "whatsapp".to_string(),
            alias: "cloud".to_string(),
            owning_agent: None,
            enabled: true,
        };
        let ambiguous = zeroclaw_config::schema::ChannelAliasInfo {
            channel_type: "whatsapp".to_string(),
            alias: "ambiguous".to_string(),
            owning_agent: None,
            enabled: true,
        };
        let discord = zeroclaw_config::schema::ChannelAliasInfo {
            channel_type: "discord".to_string(),
            alias: "default".to_string(),
            owning_agent: None,
            enabled: true,
        };

        assert_eq!(
            compiled_readiness_key_for_alias(&config, &web),
            "whatsapp-web"
        );
        assert_eq!(
            compiled_readiness_key_for_alias(&config, &cloud),
            "whatsapp"
        );
        assert_eq!(
            compiled_readiness_key_for_alias(&config, &ambiguous),
            "whatsapp",
            "ambiguous WhatsApp configs follow runtime Cloud precedence"
        );
        assert_eq!(
            compiled_readiness_key_for_alias(&config, &discord),
            "discord"
        );
    }

    #[cfg(not(feature = "channel-nextcloud"))]
    #[tokio::test]
    async fn api_channels_marks_configured_uncompiled_channel_unavailable() {
        let mut config = zeroclaw_config::schema::Config::default();
        config.channels.nextcloud_talk.insert(
            "default".to_string(),
            zeroclaw_config::schema::NextcloudTalkConfig {
                enabled: true,
                base_url: "https://cloud.example.com".to_string(),
                app_token: "test-token".to_string(),
                ..Default::default()
            },
        );

        let response = handle_api_channels(State(test_state(config)), HeaderMap::new())
            .await
            .into_response();
        let json = response_json(response).await;
        let channels = json["channels"].as_array().expect("channels array");
        let nextcloud = channels
            .iter()
            .find(|channel| channel["alias"] == "default")
            .expect("configured channel is listed");

        assert!(
            matches!(
                nextcloud["type"].as_str(),
                Some("nextcloud-talk" | "nextcloud_talk")
            ),
            "unexpected channel type: {}",
            nextcloud["type"]
        );
        assert_eq!(nextcloud["enabled"], true);
        assert_eq!(nextcloud["compiled"], false);
        assert_eq!(nextcloud["status"], "not_compiled");
        assert_eq!(nextcloud["health"], "unavailable");
    }

    /// Bind `channel_ref` (e.g. `"wechat.admin"`) to an enabled agent so
    /// readiness reaches the authenticated/listening probes.
    fn bind_channel_to_agent(config: &mut zeroclaw_config::schema::Config, channel_ref: &str) {
        config.agents.insert(
            "rowan".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                channels: vec![zeroclaw_config::providers::ChannelRef::new(
                    channel_ref.to_string(),
                )],
                ..Default::default()
            },
        );
    }

    #[cfg(feature = "channel-wechat")]
    #[tokio::test]
    async fn api_channels_wechat_authenticated_tracks_persisted_login() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = zeroclaw_config::schema::Config::default();
        config.gateway.require_pairing = false;
        config.channels.wechat.insert(
            "admin".to_string(),
            zeroclaw_config::schema::WeChatConfig {
                enabled: true,
                state_dir: Some(temp.path().to_string_lossy().into_owned()),
                ..Default::default()
            },
        );
        bind_channel_to_agent(&mut config, "wechat.admin");

        // Unpaired: nothing persisted in the channel's state dir.
        let response = handle_api_channels(State(test_state(config.clone())), HeaderMap::new())
            .await
            .into_response();
        let json = response_json(response).await;
        let channel = json["channels"]
            .as_array()
            .expect("channels array")
            .iter()
            .find(|channel| channel["name"] == "wechat.admin")
            .cloned()
            .expect("wechat channel is listed");
        assert_eq!(channel["readiness"]["authenticated"], "missing");
        assert_eq!(channel["status"], "error");
        assert_eq!(channel["health"], "down");
        assert!(
            channel["readiness"]["requirements"]
                .as_array()
                .expect("requirements array")
                .iter()
                .any(|item| item
                    .as_str()
                    .is_some_and(|s| s.contains("Pair this channel")))
        );

        // Paired: the channel's own persisted login (account.json token).
        std::fs::write(
            temp.path().join("account.json"),
            r#"{"token": "tok_persisted", "account_id": "acct_1"}"#,
        )
        .unwrap();
        let response = handle_api_channels(State(test_state(config)), HeaderMap::new())
            .await
            .into_response();
        let json = response_json(response).await;
        let channel = json["channels"]
            .as_array()
            .expect("channels array")
            .iter()
            .find(|channel| channel["name"] == "wechat.admin")
            .cloned()
            .expect("wechat channel is listed");
        assert_eq!(channel["readiness"]["authenticated"], "ready");
        // Listener liveness is still unprobed, so the summary stays
        // conservative rather than claiming the channel is up.
        assert_eq!(channel["readiness"]["listening"], "unknown");
        assert_eq!(channel["status"], "unknown");
    }

    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn api_channels_whatsapp_web_unpaired_reports_missing_auth_without_touching_disk() {
        let temp = tempfile::tempdir().unwrap();
        let session_path = temp.path().join("session.db");
        let mut config = zeroclaw_config::schema::Config::default();
        config.gateway.require_pairing = false;
        config.channels.whatsapp.insert(
            "admin".to_string(),
            zeroclaw_config::schema::WhatsAppConfig {
                enabled: true,
                session_path: Some(session_path.to_string_lossy().into_owned()),
                ..Default::default()
            },
        );
        bind_channel_to_agent(&mut config, "whatsapp.admin");

        let response = handle_api_channels(State(test_state(config)), HeaderMap::new())
            .await
            .into_response();
        let json = response_json(response).await;
        let channel = json["channels"]
            .as_array()
            .expect("channels array")
            .iter()
            .find(|channel| channel["name"] == "whatsapp.admin")
            .cloned()
            .expect("whatsapp channel is listed");
        assert_eq!(channel["readiness"]["authenticated"], "missing");
        assert_eq!(channel["status"], "error");
        assert!(
            !session_path.exists(),
            "the readiness probe must never create the session database"
        );
    }

    #[tokio::test]
    async fn api_channels_without_login_probe_keeps_authenticated_unknown() {
        let mut config = config_with_telegram("default");
        bind_channel_to_agent(&mut config, "telegram.default");

        let response = handle_api_channels(State(test_state(config)), HeaderMap::new())
            .await
            .into_response();
        let json = response_json(response).await;
        let channel = json["channels"]
            .as_array()
            .expect("channels array")
            .iter()
            .find(|channel| channel["name"] == "telegram.default")
            .cloned()
            .expect("telegram channel is listed");
        assert_eq!(channel["readiness"]["authenticated"], "unknown");
        assert!(
            channel["readiness"]["notes"]
                .as_array()
                .expect("notes array")
                .iter()
                .any(|note| {
                    note.as_str()
                        .is_some_and(|s| s.contains("not checked for `telegram`"))
                })
        );
    }

    #[cfg(feature = "channel-wechat")]
    #[tokio::test]
    async fn api_channel_relink_wechat_clears_persisted_login_then_noops() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = zeroclaw_config::schema::Config::default();
        config.gateway.require_pairing = false;
        config.channels.wechat.insert(
            "admin".to_string(),
            zeroclaw_config::schema::WeChatConfig {
                enabled: true,
                state_dir: Some(temp.path().to_string_lossy().into_owned()),
                ..Default::default()
            },
        );
        std::fs::write(
            temp.path().join("account.json"),
            r#"{"token": "tok_persisted", "account_id": "acct_1"}"#,
        )
        .unwrap();
        std::fs::write(temp.path().join("sync.json"), r#"{"get_updates_buf": "c"}"#).unwrap();

        let response = handle_api_channel_relink(
            State(test_state(config.clone())),
            Path("wechat.admin".to_string()),
            HeaderMap::new(),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let json = response_json(response).await;
        assert_eq!(json["outcome"], "cleared");
        assert_eq!(json["restart_required"], true);
        assert_eq!(json["removed"].as_array().expect("removed array").len(), 2);
        assert!(!temp.path().join("account.json").exists());
        assert!(!temp.path().join("sync.json").exists());

        // Relinking again is the documented no-op.
        let response = handle_api_channel_relink(
            State(test_state(config)),
            Path("wechat.admin".to_string()),
            HeaderMap::new(),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let json = response_json(response).await;
        assert_eq!(json["outcome"], "nothing_to_clear");
        assert_eq!(json["restart_required"], false);
    }

    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn api_channel_relink_whatsapp_web_unpaired_noops_without_touching_disk() {
        let temp = tempfile::tempdir().unwrap();
        let session_path = temp.path().join("session.db");
        let mut config = zeroclaw_config::schema::Config::default();
        config.gateway.require_pairing = false;
        config.channels.whatsapp.insert(
            "admin".to_string(),
            zeroclaw_config::schema::WhatsAppConfig {
                enabled: true,
                session_path: Some(session_path.to_string_lossy().into_owned()),
                ..Default::default()
            },
        );

        let response = handle_api_channel_relink(
            State(test_state(config)),
            Path("whatsapp.admin".to_string()),
            HeaderMap::new(),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let json = response_json(response).await;
        assert_eq!(json["outcome"], "nothing_to_clear");
        assert!(
            !session_path.exists(),
            "relinking an unpaired channel must not create the session database"
        );
    }

    #[tokio::test]
    async fn api_channel_relink_unsupported_channel_is_explicit_conflict_noop() {
        let config = config_with_telegram("default");

        let response = handle_api_channel_relink(
            State(test_state(config)),
            Path("telegram.default".to_string()),
            HeaderMap::new(),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let json = response_json(response).await;
        assert_eq!(json["outcome"], "unsupported");
        assert!(
            json["error"]
                .as_str()
                .expect("error string")
                .contains("nothing was changed")
        );
    }

    #[tokio::test]
    async fn api_channel_relink_unknown_channel_is_not_found() {
        let response = handle_api_channel_relink(
            State(test_state(zeroclaw_config::schema::Config::default())),
            Path("wechat.ghost".to_string()),
            HeaderMap::new(),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn api_channel_relink_requires_bearer_auth_when_pairing_enabled() {
        let state = AppState {
            pairing: Arc::new(PairingGuard::new(true, &[])),
            ..test_state(config_with_telegram("default"))
        };

        let response = handle_api_channel_relink(
            State(state),
            Path("telegram.default".to_string()),
            HeaderMap::new(),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    fn link_job_to_test_agent(state: &AppState, job_id: &str) {
        state
            .config
            .write()
            .agents
            .get_mut("test-agent")
            .expect("test-agent configured by with_test_agent")
            .cron_jobs
            .push(job_id.to_string());
    }

    fn config_with_webhook(
        alias: &str,
        enabled: bool,
        bound: bool,
        port: u16,
        listen_path: Option<&str>,
    ) -> zeroclaw_config::schema::Config {
        let mut config = zeroclaw_config::schema::Config::default();
        config.gateway.port = 42617;
        config.gateway.require_pairing = false;
        config.channels.webhook.insert(
            alias.to_string(),
            zeroclaw_config::schema::WebhookConfig {
                enabled,
                port,
                listen_path: listen_path.map(ToString::to_string),
                ..Default::default()
            },
        );
        if bound {
            config.agents.insert(
                "rowan".to_string(),
                zeroclaw_config::schema::AliasedAgentConfig {
                    channels: vec![zeroclaw_config::providers::ChannelRef::new(format!(
                        "webhook.{alias}"
                    ))],
                    ..Default::default()
                },
            );
        }
        config
    }

    fn config_with_telegram(alias: &str) -> zeroclaw_config::schema::Config {
        let mut config = zeroclaw_config::schema::Config::default();
        config.channels.telegram.insert(
            alias.to_string(),
            zeroclaw_config::schema::TelegramConfig {
                enabled: true,
                bot_token: "test-token".to_string(),
                ..Default::default()
            },
        );
        config.agents.insert(
            "rowan".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                channels: vec![zeroclaw_config::providers::ChannelRef::new(format!(
                    "telegram.{alias}"
                ))],
                ..Default::default()
            },
        );
        config
    }

    fn first_channel_info(
        config: &zeroclaw_config::schema::Config,
    ) -> zeroclaw_config::schema::ChannelAliasInfo {
        config
            .channels_by_alias()
            .into_iter()
            .next()
            .expect("channel alias should be present")
    }

    #[test]
    fn channel_readiness_webhook_does_not_call_gateway_route_healthy_without_listener() {
        let config = config_with_webhook("default", true, true, 42617, Some("/webhook"));
        let state = test_state(config.clone());
        let health = zeroclaw_runtime::health::snapshot();
        let info = first_channel_info(&config);
        let readiness = channel_readiness(&config, &info, &health, &state);

        assert_eq!(readiness.authenticated, ChannelReadinessState::Ready);
        assert_eq!(readiness.listening, ChannelReadinessState::Missing);
        assert_eq!(channel_readiness_summary(&readiness), ("error", "down"));
        assert!(
            readiness
                .requirements
                .iter()
                .any(|item| item.contains("Start a channel listener"))
        );
    }

    #[test]
    fn channel_readiness_webhook_does_not_call_custom_path_healthy_without_listener() {
        let config = config_with_webhook("custom_path", true, true, 42632, Some("/eyrie"));
        let state = test_state(config.clone());
        let health = zeroclaw_runtime::health::snapshot();
        let info = first_channel_info(&config);
        let readiness = channel_readiness(&config, &info, &health, &state);

        assert_eq!(readiness.authenticated, ChannelReadinessState::Ready);
        assert_eq!(readiness.listening, ChannelReadinessState::Missing);
        assert_eq!(channel_readiness_summary(&readiness), ("error", "down"));
        assert!(
            readiness
                .requirements
                .iter()
                .any(|item| item.contains("Start a channel listener"))
        );
    }

    #[test]
    fn channel_readiness_webhook_uses_supervised_listener_health_for_custom_path() {
        let config = config_with_webhook("supervised", true, true, 42632, Some("/eyrie"));
        zeroclaw_runtime::health::mark_component_ok("channel:webhook.supervised");
        let state = test_state(config.clone());
        let health = zeroclaw_runtime::health::snapshot();
        let info = first_channel_info(&config);
        let readiness = channel_readiness(&config, &info, &health, &state);

        assert_eq!(readiness.listening, ChannelReadinessState::Ready);
        assert_eq!(channel_readiness_summary(&readiness), ("active", "healthy"));
    }

    #[test]
    fn channel_readiness_webhook_rejects_stale_listener_health() {
        let config = config_with_webhook("stale", true, true, 42632, Some("/eyrie"));
        let component = "channel:webhook.stale".to_string();
        let old = (chrono::Utc::now()
            - chrono::Duration::seconds(CHANNEL_LISTENER_HEALTH_MAX_AGE_SECS + 5))
        .to_rfc3339();
        let health = zeroclaw_runtime::health::HealthSnapshot {
            pid: std::process::id(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            uptime_seconds: 1,
            components: std::collections::BTreeMap::from([(
                component,
                zeroclaw_runtime::health::ComponentHealth {
                    status: "ok".to_string(),
                    updated_at: old,
                    last_ok: None,
                    last_error: None,
                    restart_count: 0,
                },
            )]),
        };
        let state = test_state(config.clone());
        let info = first_channel_info(&config);
        let readiness = channel_readiness(&config, &info, &health, &state);

        assert_eq!(readiness.listening, ChannelReadinessState::Missing);
        assert_eq!(channel_readiness_summary(&readiness), ("error", "down"));
    }

    #[test]
    fn channel_readiness_webhook_uses_live_pairing_guard_for_auth() {
        let config = config_with_webhook("paired", true, true, 42632, Some("/eyrie"));
        zeroclaw_runtime::health::mark_component_ok("channel:webhook.paired");
        let mut state = test_state(config.clone());
        state.pairing = Arc::new(PairingGuard::new(true, &[]));
        let health = zeroclaw_runtime::health::snapshot();
        let info = first_channel_info(&config);
        let readiness = channel_readiness(&config, &info, &health, &state);

        assert_eq!(readiness.authenticated, ChannelReadinessState::Missing);
        assert_eq!(readiness.listening, ChannelReadinessState::Ready);
        assert_eq!(channel_readiness_summary(&readiness), ("error", "down"));
    }

    #[test]
    fn channel_readiness_unchecked_channel_types_are_unknown_not_down() {
        let config = config_with_telegram("ops");
        let state = test_state(config.clone());
        let health = zeroclaw_runtime::health::snapshot();
        let info = first_channel_info(&config);
        let readiness = channel_readiness(&config, &info, &health, &state);

        assert_eq!(readiness.enabled, ChannelReadinessState::Ready);
        assert_eq!(readiness.bound_to_agent, ChannelReadinessState::Ready);
        assert_eq!(readiness.authenticated, ChannelReadinessState::Unknown);
        assert_eq!(readiness.listening, ChannelReadinessState::Unknown);
        assert_eq!(
            channel_readiness_summary(&readiness),
            ("unknown", "degraded")
        );
        assert!(readiness.requirements.is_empty());
        assert!(
            readiness
                .notes
                .iter()
                .any(|item| item.contains("not checked"))
        );
    }

    #[test]
    fn channel_readiness_orphan_channel_reports_missing_agent_binding_without_broken_health() {
        let config = config_with_webhook("orphan", true, false, 42617, Some("/webhook"));
        let state = test_state(config.clone());
        let health = zeroclaw_runtime::health::snapshot();
        let info = first_channel_info(&config);
        let readiness = channel_readiness(&config, &info, &health, &state);

        assert_eq!(readiness.bound_to_agent, ChannelReadinessState::Missing);
        assert_eq!(readiness.listening, ChannelReadinessState::Unknown);
        assert_eq!(
            channel_readiness_summary(&readiness),
            ("inactive", "degraded")
        );
        assert!(
            readiness
                .requirements
                .iter()
                .any(|item| item.contains("Bind this channel"))
        );
    }

    #[test]
    fn require_auth_rejects_empty_bearer_token() {
        let config = zeroclaw_config::schema::Config::default();
        let mut state = test_state(config);
        state.pairing = Arc::new(PairingGuard::new(true, &[]));

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "Bearer ".parse().unwrap(), // empty token after prefix
        );

        let result = require_auth(&state, &headers);
        assert!(result.is_err(), "empty bearer token must be rejected");
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn api_channels_serializes_readiness_without_duplicate_summary_fields() {
        let config = config_with_webhook("ops", true, true, 42617, Some("/webhook"));
        let state = test_state(config);

        let response = handle_api_channels(State(state), HeaderMap::new())
            .await
            .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let json = response_json(response).await;
        let channel = &json["channels"][0];
        let webhook_compiled = zeroclaw_channels::listing::is_channel_type_compiled("webhook");
        assert_eq!(channel["name"], "webhook.ops");
        assert_eq!(channel["compiled"], webhook_compiled);
        if webhook_compiled {
            assert_eq!(channel["status"], "error");
            assert_eq!(channel["health"], "down");
        } else {
            assert_eq!(channel["status"], "not_compiled");
            assert_eq!(channel["health"], "unavailable");
        }
        assert_eq!(channel["readiness"]["enabled"], "ready");
        assert_eq!(channel["readiness"]["authenticated"], "ready");
        assert_eq!(channel["readiness"]["listening"], "missing");
        assert!(channel["readiness"].get("configured").is_none());
        assert!(channel["readiness"].get("status").is_none());
        assert!(channel["readiness"].get("health").is_none());
    }

    fn test_state_with_session_backend(
        config: zeroclaw_config::schema::Config,
        backend: Arc<dyn SessionBackend>,
    ) -> AppState {
        let mut state = test_state(config);
        state.session_backend = Some(backend);
        state
    }

    #[tokio::test]
    async fn session_message_post_persists_and_broadcasts_to_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let backend: Arc<dyn SessionBackend> = Arc::new(SessionStore::new(tmp.path()).unwrap());
        backend
            .append(
                "gw_operator-1",
                &zeroclaw_providers::ChatMessage::assistant("existing"),
            )
            .unwrap();
        let state = test_state_with_session_backend(config, backend.clone());
        let mut rx = state.event_tx.subscribe();

        let response = handle_api_session_message_post(
            State(state.clone()),
            HeaderMap::new(),
            Path("operator-1".to_string()),
            Json(
                serde_json::from_value::<SessionMessagePostBody>(serde_json::json!({
                    "content": "deploy finished"
                }))
                .expect("body should deserialize"),
            ),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let json = response_json(response).await;
        assert_eq!(json["status"], "ok");
        assert_eq!(json["session_id"], "operator-1");
        assert_eq!(json["message"]["role"], "assistant");
        assert_eq!(json["message"]["content"], "deploy finished");
        assert!(json.get("message_count").is_none());

        let messages = backend.load("gw_operator-1");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].content, "deploy finished");

        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("broadcast event")
            .expect("broadcast value");
        assert_eq!(event["type"], "message");
        assert_eq!(event["session_id"], "operator-1");
        assert_eq!(event["role"], "assistant");
        assert_eq!(event["content"], "deploy finished");

        let history = state.event_buffer.snapshot();
        assert!(
            history.is_empty(),
            "session-scoped chat messages stay out of global event history"
        );
    }

    #[tokio::test]
    async fn session_message_post_rejects_empty_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let backend: Arc<dyn SessionBackend> = Arc::new(SessionStore::new(tmp.path()).unwrap());
        let state = test_state_with_session_backend(config, backend);

        let response = handle_api_session_message_post(
            State(state),
            HeaderMap::new(),
            Path("operator-1".to_string()),
            Json(
                serde_json::from_value::<SessionMessagePostBody>(serde_json::json!({
                    "content": "   "
                }))
                .expect("body should deserialize"),
            ),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let json = response_json(response).await;
        assert_eq!(json["error"], "content is required");
    }

    #[tokio::test]
    async fn session_message_post_rejects_unknown_session_without_creating_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let backend: Arc<dyn SessionBackend> = Arc::new(SessionStore::new(tmp.path()).unwrap());
        let state = test_state_with_session_backend(config, backend.clone());

        let response = handle_api_session_message_post(
            State(state),
            HeaderMap::new(),
            Path("operator-1".to_string()),
            Json(
                serde_json::from_value::<SessionMessagePostBody>(serde_json::json!({
                    "content": "deploy finished"
                }))
                .expect("body should deserialize"),
            ),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let json = response_json(response).await;
        assert_eq!(json["error"], "Session not found");
        assert!(backend.load("gw_operator-1").is_empty());
    }

    #[tokio::test]
    async fn session_message_post_waits_for_session_queue_before_append() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let backend: Arc<dyn SessionBackend> = Arc::new(SessionStore::new(tmp.path()).unwrap());
        backend
            .append(
                "gw_operator-1",
                &zeroclaw_providers::ChatMessage::assistant("existing"),
            )
            .unwrap();
        let state = test_state_with_session_backend(config, backend.clone());
        let session_guard = state.session_queue.acquire("gw_operator-1").await.unwrap();

        let response_fut = handle_api_session_message_post(
            State(state),
            HeaderMap::new(),
            Path("operator-1".to_string()),
            Json(
                serde_json::from_value::<SessionMessagePostBody>(serde_json::json!({
                    "content": "queued notification"
                }))
                .expect("body should deserialize"),
            ),
        );
        tokio::pin!(response_fut);

        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut response_fut)
                .await
                .is_err(),
            "POST should wait behind the active session queue guard"
        );
        assert_eq!(backend.load("gw_operator-1").len(), 1);

        drop(session_guard);
        let response = tokio::time::timeout(Duration::from_secs(1), response_fut)
            .await
            .expect("queued POST should complete")
            .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let messages = backend.load("gw_operator-1");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content, "queued notification");
    }

    #[tokio::test]
    async fn cron_api_shell_roundtrip_includes_delivery() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let state = test_state(with_test_agent(config));

        let add_response = handle_api_cron_add(
            State(state.clone()),
            HeaderMap::new(),
            Json(
                serde_json::from_value::<CronAddBody>(serde_json::json!({
                    "name": "test-job",
                    "agent": "test-agent",
                    "schedule": "*/5 * * * *",
                    "command": "echo hello",
                    "delivery": {
                        "mode": "announce",
                        "channel": "discord",
                        "to": "1234567890",
                        "best_effort": true
                    }
                }))
                .expect("body should deserialize"),
            ),
        )
        .await
        .into_response();

        let add_json = response_json(add_response).await;
        assert_eq!(add_json["status"], "ok");
        assert_eq!(add_json["job"]["delivery"]["mode"], "announce");
        assert_eq!(add_json["job"]["delivery"]["channel"], "discord");
        assert_eq!(add_json["job"]["delivery"]["to"], "1234567890");

        let list_response = handle_api_cron_list(State(state), HeaderMap::new())
            .await
            .into_response();
        let list_json = response_json(list_response).await;
        let jobs = list_json["jobs"].as_array().expect("jobs array");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0]["delivery"]["mode"], "announce");
        assert_eq!(jobs[0]["delivery"]["channel"], "discord");
        assert_eq!(jobs[0]["delivery"]["to"], "1234567890");
    }

    #[tokio::test]
    async fn cron_api_accepts_agent_jobs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let state = test_state(with_test_agent(config));

        let response = handle_api_cron_add(
            State(state.clone()),
            HeaderMap::new(),
            Json(
                serde_json::from_value::<CronAddBody>(serde_json::json!({
                    "name": "agent-job",
                    "agent": "test-agent",
                    "schedule": "*/5 * * * *",
                    "job_type": "agent",
                    "command": "ignored shell command",
                    "prompt": "summarize the latest logs"
                }))
                .expect("body should deserialize"),
            ),
        )
        .await
        .into_response();

        let json = response_json(response).await;
        assert_eq!(json["status"], "ok");

        let config = state.config.read().clone();
        let jobs = zeroclaw_runtime::cron::list_jobs(&config).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_type, zeroclaw_runtime::cron::JobType::Agent);
        assert_eq!(jobs[0].prompt.as_deref(), Some("summarize the latest logs"));
    }

    #[tokio::test]
    async fn cron_api_timezone_add_persists_explicit_timezone() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let state = test_state(with_test_agent(config));

        let response = handle_api_cron_add(
            State(state.clone()),
            HeaderMap::new(),
            Json(
                serde_json::from_value::<CronAddBody>(serde_json::json!({
                    "agent": "test-agent",
                    "name": "localized-job",
                    "schedule": "0 9 * * *",
                    "tz": "America/New_York",
                    "command": "echo hello"
                }))
                .expect("body should deserialize"),
            ),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let config = state.config.read().clone();
        let jobs = zeroclaw_runtime::cron::list_jobs(&config).unwrap();
        assert_eq!(
            jobs[0].schedule,
            zeroclaw_runtime::cron::Schedule::Cron {
                expr: "0 9 * * *".to_string(),
                tz: Some("America/New_York".to_string()),
            }
        );
    }

    #[tokio::test]
    async fn cron_api_timezone_add_rejects_invalid_timezone_as_bad_request() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let state = test_state(with_test_agent(config));

        let response = handle_api_cron_add(
            State(state),
            HeaderMap::new(),
            Json(
                serde_json::from_value::<CronAddBody>(serde_json::json!({
                    "agent": "test-agent",
                    "name": "invalid-timezone-job",
                    "schedule": "0 9 * * *",
                    "tz": "Invalid/Zone",
                    "command": "echo hello"
                }))
                .expect("body should deserialize"),
            ),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let json = response_json(response).await;
        assert!(
            json["error"]
                .as_str()
                .unwrap_or_default()
                .contains("Invalid IANA timezone")
        );
    }

    #[tokio::test]
    async fn cron_api_timezone_patch_schedule_preserves_existing_timezone() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let state = test_state(with_test_agent(config));
        let job = zeroclaw_runtime::cron::add_shell_job_with_approval(
            &state.config.read().clone(),
            "test-agent",
            Some("localized-job".to_string()),
            zeroclaw_runtime::cron::Schedule::Cron {
                expr: "0 9 * * *".to_string(),
                tz: Some("Europe/Berlin".to_string()),
            },
            "echo hello",
            None,
            true,
        )
        .expect("job added");

        let response = handle_api_cron_patch(
            State(state.clone()),
            HeaderMap::new(),
            Path(job.id.clone()),
            Json(
                serde_json::from_value::<CronPatchBody>(serde_json::json!({
                    "agent": "test-agent",
                    "schedule": "30 9 * * *"
                }))
                .expect("body should deserialize"),
            ),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let updated = zeroclaw_runtime::cron::get_job(&state.config.read().clone(), &job.id)
            .expect("updated job");
        assert_eq!(
            updated.schedule,
            zeroclaw_runtime::cron::Schedule::Cron {
                expr: "30 9 * * *".to_string(),
                tz: Some("Europe/Berlin".to_string()),
            }
        );
    }

    #[tokio::test]
    async fn cron_api_timezone_patch_replaces_timezone_when_provided() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let state = test_state(with_test_agent(config));
        let job = zeroclaw_runtime::cron::add_shell_job_with_approval(
            &state.config.read().clone(),
            "test-agent",
            Some("localized-job".to_string()),
            zeroclaw_runtime::cron::Schedule::Cron {
                expr: "0 9 * * *".to_string(),
                tz: Some("America/New_York".to_string()),
            },
            "echo hello",
            None,
            true,
        )
        .expect("job added");

        let response = handle_api_cron_patch(
            State(state.clone()),
            HeaderMap::new(),
            Path(job.id.clone()),
            Json(
                serde_json::from_value::<CronPatchBody>(serde_json::json!({
                    "agent": "test-agent",
                    "schedule": "30 9 * * *",
                    "tz": "Asia/Tokyo"
                }))
                .expect("body should deserialize"),
            ),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let updated = zeroclaw_runtime::cron::get_job(&state.config.read().clone(), &job.id)
            .expect("updated job");
        assert_eq!(
            updated.schedule,
            zeroclaw_runtime::cron::Schedule::Cron {
                expr: "30 9 * * *".to_string(),
                tz: Some("Asia/Tokyo".to_string()),
            }
        );
    }

    #[tokio::test]
    async fn cron_api_timezone_patch_sets_timezone_without_schedule_change() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let state = test_state(with_test_agent(config));
        let job = zeroclaw_runtime::cron::add_shell_job_with_approval(
            &state.config.read().clone(),
            "test-agent",
            Some("runtime-local-job".to_string()),
            zeroclaw_runtime::cron::Schedule::Cron {
                expr: "0 9 * * *".to_string(),
                tz: None,
            },
            "echo hello",
            None,
            true,
        )
        .expect("job added");

        let response = handle_api_cron_patch(
            State(state.clone()),
            HeaderMap::new(),
            Path(job.id.clone()),
            Json(
                serde_json::from_value::<CronPatchBody>(serde_json::json!({
                    "agent": "test-agent",
                    "tz": "America/Chicago"
                }))
                .expect("body should deserialize"),
            ),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let updated = zeroclaw_runtime::cron::get_job(&state.config.read().clone(), &job.id)
            .expect("updated job");
        assert_eq!(
            updated.schedule,
            zeroclaw_runtime::cron::Schedule::Cron {
                expr: "0 9 * * *".to_string(),
                tz: Some("America/Chicago".to_string()),
            }
        );
    }

    #[tokio::test]
    async fn cron_api_timezone_patch_rejects_invalid_timezone_as_bad_request() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let state = test_state(with_test_agent(config));
        let job = zeroclaw_runtime::cron::add_shell_job_with_approval(
            &state.config.read().clone(),
            "test-agent",
            Some("localized-job".to_string()),
            zeroclaw_runtime::cron::Schedule::Cron {
                expr: "0 9 * * *".to_string(),
                tz: Some("America/New_York".to_string()),
            },
            "echo hello",
            None,
            true,
        )
        .expect("job added");

        let response = handle_api_cron_patch(
            State(state),
            HeaderMap::new(),
            Path(job.id),
            Json(
                serde_json::from_value::<CronPatchBody>(serde_json::json!({
                    "agent": "test-agent",
                    "tz": "Invalid/Zone"
                }))
                .expect("body should deserialize"),
            ),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let json = response_json(response).await;
        assert!(
            json["error"]
                .as_str()
                .unwrap_or_default()
                .contains("Invalid IANA timezone")
        );
    }

    #[tokio::test]
    async fn cron_api_timezone_patch_clears_timezone_with_explicit_signal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let state = test_state(with_test_agent(config));
        let job = zeroclaw_runtime::cron::add_shell_job_with_approval(
            &state.config.read().clone(),
            "test-agent",
            Some("localized-job".to_string()),
            zeroclaw_runtime::cron::Schedule::Cron {
                expr: "0 9 * * *".to_string(),
                tz: Some("America/New_York".to_string()),
            },
            "echo hello",
            None,
            true,
        )
        .expect("job added");

        let response = handle_api_cron_patch(
            State(state.clone()),
            HeaderMap::new(),
            Path(job.id.clone()),
            Json(
                serde_json::from_value::<CronPatchBody>(serde_json::json!({
                    "agent": "test-agent",
                    "clear_tz": true
                }))
                .expect("body should deserialize"),
            ),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let updated = zeroclaw_runtime::cron::get_job(&state.config.read().clone(), &job.id)
            .expect("updated job");
        assert_eq!(
            updated.schedule,
            zeroclaw_runtime::cron::Schedule::Cron {
                expr: "0 9 * * *".to_string(),
                tz: None,
            }
        );
    }

    #[tokio::test]
    async fn cron_api_patch_enabled_without_agent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let state = test_state(with_test_agent(config));
        let job = zeroclaw_runtime::cron::add_shell_job_with_approval(
            &state.config.read().clone(),
            "test-agent",
            Some("toggle-job".to_string()),
            zeroclaw_runtime::cron::Schedule::Cron {
                expr: "*/5 * * * *".to_string(),
                tz: None,
            },
            "echo hello",
            None,
            true,
        )
        .expect("job added");

        // No `agent` field at all — pause/resume must not require one.
        let response = handle_api_cron_patch(
            State(state.clone()),
            HeaderMap::new(),
            Path(job.id.clone()),
            Json(
                serde_json::from_value::<CronPatchBody>(serde_json::json!({ "enabled": false }))
                    .expect("body should deserialize"),
            ),
        )
        .await
        .into_response();

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "enable/disable toggle must not require an agent"
        );
        let updated = zeroclaw_runtime::cron::get_job(&state.config.read().clone(), &job.id)
            .expect("updated job");
        assert!(!updated.enabled, "job should be disabled after the patch");
    }

    #[tokio::test]
    async fn cron_api_patch_name_and_schedule_without_agent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let state = test_state(with_test_agent(config));
        let job = zeroclaw_runtime::cron::add_shell_job_with_approval(
            &state.config.read().clone(),
            "test-agent",
            Some("old-name".to_string()),
            zeroclaw_runtime::cron::Schedule::Cron {
                expr: "*/5 * * * *".to_string(),
                tz: None,
            },
            "echo hello",
            None,
            true,
        )
        .expect("job added");

        // Metadata-only patch (no command/prompt) — agent must be optional.
        let response = handle_api_cron_patch(
            State(state.clone()),
            HeaderMap::new(),
            Path(job.id.clone()),
            Json(
                serde_json::from_value::<CronPatchBody>(serde_json::json!({
                    "name": "new-name",
                    "schedule": "30 9 * * *"
                }))
                .expect("body should deserialize"),
            ),
        )
        .await
        .into_response();

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "name/schedule patch must not require an agent"
        );
        let updated = zeroclaw_runtime::cron::get_job(&state.config.read().clone(), &job.id)
            .expect("updated job");
        assert_eq!(updated.name.as_deref(), Some("new-name"));
        assert_eq!(
            updated.schedule,
            zeroclaw_runtime::cron::Schedule::Cron {
                expr: "30 9 * * *".to_string(),
                tz: None,
            }
        );
    }

    #[tokio::test]
    async fn cron_api_patch_shell_command_requires_known_agent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let state = test_state(with_test_agent(config));
        let job = zeroclaw_runtime::cron::add_shell_job_with_approval(
            &state.config.read().clone(),
            "test-agent",
            Some("shell-job".to_string()),
            zeroclaw_runtime::cron::Schedule::Cron {
                expr: "*/5 * * * *".to_string(),
                tz: None,
            },
            "echo hello",
            None,
            true,
        )
        .expect("job added");

        // Setting a shell `command` still hits the risk gate: a missing agent
        // must be a clean 400, not a fall-through.
        let response = handle_api_cron_patch(
            State(state.clone()),
            HeaderMap::new(),
            Path(job.id.clone()),
            Json(
                serde_json::from_value::<CronPatchBody>(
                    serde_json::json!({ "command": "echo bye" }),
                )
                .expect("body should deserialize"),
            ),
        )
        .await
        .into_response();

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "shell command patch with no agent must be rejected at the risk gate"
        );
        let json = response_json(response).await;
        assert!(
            json["error"]
                .as_str()
                .unwrap_or_default()
                .contains("Unknown agent"),
            "error should name the unknown agent"
        );
    }

    #[tokio::test]
    async fn cron_api_patch_shell_prompt_unknown_agent_is_bad_request_not_500() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let state = test_state(with_test_agent(config));
        let job = zeroclaw_runtime::cron::add_shell_job_with_approval(
            &state.config.read().clone(),
            "test-agent",
            Some("shell-job".to_string()),
            zeroclaw_runtime::cron::Schedule::Cron {
                expr: "*/5 * * * *".to_string(),
                tz: None,
            },
            "echo hello",
            None,
            true,
        )
        .expect("job added");

        // For a shell job a new command can arrive via `prompt`; it still routes
        // through the command-risk gate, so an unknown agent is a 400 — not the
        // 500 that an unguarded path would surface.
        let response = handle_api_cron_patch(
            State(state.clone()),
            HeaderMap::new(),
            Path(job.id.clone()),
            Json(
                serde_json::from_value::<CronPatchBody>(serde_json::json!({
                    "agent": "ghost",
                    "prompt": "echo bye"
                }))
                .expect("body should deserialize"),
            ),
        )
        .await
        .into_response();

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "shell-job prompt with unknown agent must be 400, not 500"
        );
    }

    #[tokio::test]
    async fn cron_api_patch_agent_prompt_without_agent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let state = test_state(with_test_agent(config));

        let add_response = handle_api_cron_add(
            State(state.clone()),
            HeaderMap::new(),
            Json(
                serde_json::from_value::<CronAddBody>(serde_json::json!({
                    "name": "agent-job",
                    "agent": "test-agent",
                    "schedule": "*/5 * * * *",
                    "job_type": "agent",
                    "command": "ignored",
                    "prompt": "old prompt"
                }))
                .expect("body should deserialize"),
            ),
        )
        .await
        .into_response();
        assert_eq!(add_response.status(), StatusCode::OK);
        let id = zeroclaw_runtime::cron::list_jobs(&state.config.read().clone()).unwrap()[0]
            .id
            .clone();

        // For an agent-type job `prompt` is an LLM prompt, not a shell command,
        // so it is not agent-gated and may omit `agent`.
        let response = handle_api_cron_patch(
            State(state.clone()),
            HeaderMap::new(),
            Path(id.clone()),
            Json(
                serde_json::from_value::<CronPatchBody>(
                    serde_json::json!({ "prompt": "new prompt" }),
                )
                .expect("body should deserialize"),
            ),
        )
        .await
        .into_response();

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "agent-type prompt patch must not require an agent"
        );
        let updated = zeroclaw_runtime::cron::get_job(&state.config.read().clone(), &id)
            .expect("updated job");
        assert_eq!(updated.prompt.as_deref(), Some("new prompt"));
    }

    #[tokio::test]
    async fn cron_api_rejects_announce_delivery_without_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let state = test_state(with_test_agent(config));

        let response = handle_api_cron_add(
            State(state.clone()),
            HeaderMap::new(),
            Json(
                serde_json::from_value::<CronAddBody>(serde_json::json!({
                    "name": "invalid-delivery-job",
                    "agent": "test-agent",
                    "schedule": "*/5 * * * *",
                    "command": "echo hello",
                    "delivery": {
                        "mode": "announce",
                        "channel": "discord"
                    }
                }))
                .expect("body should deserialize"),
            ),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let json = response_json(response).await;
        assert!(
            json["error"]
                .as_str()
                .unwrap_or_default()
                .contains("delivery.to is required")
        );

        let config = state.config.read().clone();
        assert!(
            zeroclaw_runtime::cron::list_jobs(&config)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn cron_api_run_executes_shell_job_and_records_run() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let state = test_state(with_test_agent(config));

        let job = zeroclaw_runtime::cron::add_shell_job_with_approval(
            &state.config.read().clone(),
            "test-agent",
            None,
            zeroclaw_runtime::cron::Schedule::Cron {
                expr: "*/5 * * * *".to_string(),
                tz: None,
            },
            "echo hello-from-manual-trigger",
            None,
            true,
        )
        .expect("job added");

        // Imperative jobs get UUID ids; the scheduler resolves owning
        // agent by reverse-lookup against `agent.cron_jobs`.
        link_job_to_test_agent(&state, &job.id);

        let response =
            handle_api_cron_run(State(state.clone()), HeaderMap::new(), Path(job.id.clone()))
                .await
                .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let json = response_json(response).await;
        assert_eq!(json["status"], "ok");
        assert_eq!(json["success"], true);
        assert_eq!(json["job_id"], job.id);
        assert!(
            json["output"]
                .as_str()
                .unwrap_or_default()
                .contains("hello-from-manual-trigger")
        );

        let runs = zeroclaw_runtime::cron::list_runs(&state.config.read().clone(), &job.id, 10)
            .expect("runs listed");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "ok");
    }

    #[tokio::test]
    async fn cron_api_run_records_best_effort_delivery_failure_as_degraded() {
        zeroclaw_runtime::cron::scheduler::register_delivery_fn(Box::new(
            |_config, channel, _target, _thread_id, _output| {
                Box::pin(async move {
                    if channel == "fail-delivery" {
                        anyhow::bail!("synthetic delivery failure");
                    }
                    Ok(())
                })
            },
        ));

        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let state = test_state(with_test_agent(config));

        let job = zeroclaw_runtime::cron::add_shell_job_with_approval(
            &state.config.read().clone(),
            "test-agent",
            None,
            zeroclaw_runtime::cron::Schedule::Cron {
                expr: "*/5 * * * *".to_string(),
                tz: None,
            },
            "echo hello-from-manual-trigger",
            Some(zeroclaw_runtime::cron::DeliveryConfig {
                mode: "announce".into(),
                channel: Some("fail-delivery".into()),
                to: Some("123456".into()),
                thread_id: None,
                best_effort: true,
            }),
            true,
        )
        .expect("job added");
        link_job_to_test_agent(&state, &job.id);

        let response =
            handle_api_cron_run(State(state.clone()), HeaderMap::new(), Path(job.id.clone()))
                .await
                .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let json = response_json(response).await;
        assert_eq!(json["status"], "degraded");
        assert_eq!(json["success"], true);
        assert!(
            json["output"]
                .as_str()
                .unwrap_or_default()
                .contains("delivery failed:")
        );

        let config = state.config.read().clone();
        let updated = zeroclaw_runtime::cron::get_job(&config, &job.id).expect("updated job");
        assert_eq!(updated.last_status.as_deref(), Some("degraded"));
        assert!(
            updated
                .last_output
                .as_deref()
                .unwrap_or_default()
                .contains("delivery failed:")
        );

        let runs = zeroclaw_runtime::cron::list_runs(&config, &job.id, 10).expect("runs listed");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "degraded");
        assert!(
            runs[0]
                .output
                .as_deref()
                .unwrap_or_default()
                .contains("delivery failed:")
        );
    }

    #[tokio::test]
    async fn cron_api_run_returns_not_found_for_unknown_job() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let state = test_state(with_test_agent(config));

        let response = handle_api_cron_run(
            State(state),
            HeaderMap::new(),
            Path("does-not-exist".to_string()),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    use crate::api_pairing::{
        DeviceInfo, DeviceRegistry, revoke_device, rotate_token as rotate_device_token,
        submit_pairing_enhanced,
    };
    use chrono::Utc;

    async fn paired_state_with_device(tmp: &tempfile::TempDir) -> (AppState, String, String) {
        let data_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&data_dir).unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: data_dir.clone(),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };

        let pairing = Arc::new(PairingGuard::new(true, &[]));
        let code = pairing.pairing_code().unwrap();
        let token = pairing.try_pair(&code, "test").await.unwrap().unwrap();
        let token_hash = PairingGuard::token_hash(&token);

        let registry = Arc::new(DeviceRegistry::new(&data_dir));
        let device_id = "dev-1".to_string();
        registry
            .register(
                token_hash,
                DeviceInfo {
                    id: device_id.clone(),
                    name: None,
                    device_type: None,
                    paired_at: Utc::now(),
                    last_seen: Utc::now(),
                    ip_address: None,
                    capabilities: None,
                },
            )
            .expect("test device registry insert");

        let mut state = test_state(config);
        state.pairing = pairing;
        state.device_registry = Some(registry);
        (state, token, device_id)
    }

    fn bearer_headers(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        h
    }

    #[tokio::test]
    async fn reconcile_backfills_orphan_token_hashes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let registry = DeviceRegistry::new(tmp.path());

        // A real, already-registered device with a name.
        let known_hash = "a".repeat(64);
        registry
            .register(
                known_hash.clone(),
                DeviceInfo {
                    id: "known".into(),
                    name: Some("My Laptop".into()),
                    device_type: Some("desktop".into()),
                    paired_at: Utc::now(),
                    last_seen: Utc::now(),
                    ip_address: None,
                    capabilities: None,
                },
            )
            .expect("test device registry insert");

        let orphan_a = "b".repeat(64);
        let orphan_b = "c".repeat(64);
        let inserted = registry
            .reconcile_from_token_hashes(&[known_hash.clone(), orphan_a.clone(), orphan_b.clone()])
            .unwrap();
        assert_eq!(inserted, 2, "only the two orphan hashes should be inserted");
        assert_eq!(registry.device_count(), 3);

        // Existing metadata is preserved, not clobbered.
        let known = registry
            .list()
            .expect("test device registry list")
            .into_iter()
            .find(|d| d.id == "known")
            .expect("known device still present");
        assert_eq!(known.name.as_deref(), Some("My Laptop"));

        // Re-running is a no-op (idempotent).
        let again = registry
            .reconcile_from_token_hashes(&[known_hash, orphan_a, orphan_b])
            .unwrap();
        assert_eq!(again, 0);
        assert_eq!(registry.device_count(), 3);
    }

    #[tokio::test]
    async fn backfilled_orphan_is_revocable_by_its_real_hash() {
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&data_dir).unwrap();

        let pairing = PairingGuard::new(true, &[]);
        let code = pairing.pairing_code().unwrap();
        let token = pairing
            .try_pair(&code, "legacy-client")
            .await
            .unwrap()
            .unwrap();
        assert!(pairing.is_authenticated(&token));

        // Simulate the `/pair` orphan: token is paired but never registered.
        let registry = DeviceRegistry::new(&data_dir);
        assert_eq!(registry.device_count(), 0);

        let inserted = registry
            .reconcile_from_token_hashes(&pairing.tokens())
            .unwrap();
        assert_eq!(inserted, 1);

        // The backfilled row is keyed by the auth hash, so revoke returns it and
        // revoking that hash from the guard actually de-authenticates the token.
        let device = registry
            .list()
            .expect("test device registry list")
            .into_iter()
            .next()
            .expect("one backfilled device");
        let revoked_hash = registry
            .revoke(&device.id)
            .unwrap()
            .expect("device existed");
        assert_eq!(revoked_hash, PairingGuard::token_hash(&token));
        assert!(pairing.revoke_token_hash(&revoked_hash));
        assert!(
            !pairing.is_authenticated(&token),
            "token must not authenticate after revoke"
        );
    }

    #[tokio::test]
    async fn rotate_token_invalidates_old_bearer_token() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (state, old_token, device_id) = paired_state_with_device(&tmp).await;
        assert!(state.pairing.is_authenticated(&old_token));

        let response = rotate_device_token(
            State(state.clone()),
            bearer_headers(&old_token),
            Path(device_id.clone()),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);

        assert!(
            !state.pairing.is_authenticated(&old_token),
            "old bearer token must not authenticate after rotate"
        );

        let json = response_json(response).await;
        assert_eq!(json["device_id"], device_id);
        assert!(json["pairing_code"].is_string());
    }

    #[tokio::test]
    async fn rotate_token_persists_revocation_to_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (state, old_token, device_id) = paired_state_with_device(&tmp).await;
        let old_hash = PairingGuard::token_hash(&old_token);

        let response = rotate_device_token(
            State(state.clone()),
            bearer_headers(&old_token),
            Path(device_id),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);

        let persisted = state.config.read().gateway.paired_tokens.clone();
        assert!(
            !persisted.contains(&old_hash),
            "revoked token hash must not remain in gateway.paired_tokens"
        );
    }

    #[tokio::test]
    async fn submit_pairing_enhanced_persists_new_token() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (state, _old_token, _device_id) = paired_state_with_device(&tmp).await;

        let code = state
            .pairing
            .generate_new_pairing_code()
            .expect("require_pairing was enabled");

        let response = submit_pairing_enhanced(
            State(state.clone()),
            HeaderMap::new(),
            Json(serde_json::json!({ "code": code, "device_name": "repaired" })),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);

        let json = response_json(response).await;
        assert_eq!(json["persisted"], true);
        let new_token = json["token"].as_str().expect("token in response");
        let new_hash = PairingGuard::token_hash(new_token);
        assert!(
            state
                .config
                .read()
                .gateway
                .paired_tokens
                .contains(&new_hash),
            "newly paired token hash must be persisted to gateway.paired_tokens"
        );
    }

    #[tokio::test]
    async fn revoke_device_invalidates_bearer_token() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (state, old_token, device_id) = paired_state_with_device(&tmp).await;

        let response = revoke_device(
            State(state.clone()),
            bearer_headers(&old_token),
            Path(device_id),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);

        assert!(
            !state.pairing.is_authenticated(&old_token),
            "bearer token must not authenticate after device delete"
        );
        let old_hash = PairingGuard::token_hash(&old_token);
        assert!(
            !state
                .config
                .read()
                .gateway
                .paired_tokens
                .contains(&old_hash),
            "deleted device's token must be dropped from persisted paired_tokens"
        );
    }

    #[tokio::test]
    async fn rotate_unknown_device_returns_not_found() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (state, token, _) = paired_state_with_device(&tmp).await;

        let response = rotate_device_token(
            State(state.clone()),
            bearer_headers(&token),
            Path("does-not-exist".into()),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            state.pairing.is_authenticated(&token),
            "unknown-device rotate must not touch existing tokens"
        );
    }

    #[tokio::test]
    async fn rotate_with_pending_code_revokes_but_returns_null_code() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (state, token, device_id) = paired_state_with_device(&tmp).await;

        let pending_code = state
            .pairing
            .generate_new_pairing_code()
            .expect("require_pairing was enabled");

        let response = rotate_device_token(
            State(state.clone()),
            bearer_headers(&token),
            Path(device_id.clone()),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);

        assert!(
            !state.pairing.is_authenticated(&token),
            "old bearer token must be revoked even when a pairing code is pending"
        );
        assert_eq!(
            state.pairing.pairing_code().as_deref(),
            Some(pending_code.as_str()),
            "pending pairing code must survive rotate",
        );

        let json = response_json(response).await;
        assert!(json["pairing_code"].is_null());
        assert_eq!(json["device_id"], device_id);
    }

    #[tokio::test]
    async fn concurrent_rotates_do_not_both_issue_a_pairing_code() {
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&data_dir).unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: data_dir.clone(),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };

        let pairing = Arc::new(PairingGuard::new(true, &[]));
        let code = pairing.pairing_code().unwrap();
        let admin_token = pairing.try_pair(&code, "admin").await.unwrap().unwrap();

        let registry = Arc::new(DeviceRegistry::new(&data_dir));
        for id in ["dev-a", "dev-b"] {
            // Each device needs its own paired token so revoke has a hash.
            let code = pairing
                .generate_new_pairing_code()
                .expect("pairing enabled");
            let tok = pairing.try_pair(&code, id).await.unwrap().unwrap();
            registry
                .register(
                    PairingGuard::token_hash(&tok),
                    DeviceInfo {
                        id: id.to_string(),
                        name: None,
                        device_type: None,
                        paired_at: Utc::now(),
                        last_seen: Utc::now(),
                        ip_address: None,
                        capabilities: None,
                    },
                )
                .expect("test device registry insert");
        }

        let mut state = test_state(config);
        state.pairing = pairing;
        state.device_registry = Some(registry);

        let s1 = state.clone();
        let s2 = state.clone();
        let h1 = bearer_headers(&admin_token);
        let h2 = bearer_headers(&admin_token);
        let (r1, r2) = tokio::join!(
            async move {
                rotate_device_token(State(s1), h1, Path("dev-a".into()))
                    .await
                    .into_response()
            },
            async move {
                rotate_device_token(State(s2), h2, Path("dev-b".into()))
                    .await
                    .into_response()
            },
        );

        assert_eq!(r1.status(), StatusCode::OK);
        assert_eq!(r2.status(), StatusCode::OK);
        let j1 = response_json(r1).await;
        let j2 = response_json(r2).await;
        let codes_issued = usize::from(j1["pairing_code"].is_string())
            + usize::from(j2["pairing_code"].is_string());
        assert_eq!(
            codes_issued, 1,
            "exactly one of two racing rotates must win the pairing slot, \
             got {codes_issued} (j1={j1}, j2={j2})"
        );
    }

    #[cfg(feature = "a2a")]
    mod a2a_auth {
        use super::*;
        use tower::ServiceExt;

        const TOKEN: &str = "a2a-test-token";

        fn paired_state() -> AppState {
            let mut config = zeroclaw_config::schema::Config::default();
            config.a2a.server.enabled = true;
            let agent = zeroclaw_config::schema::AliasedAgentConfig {
                a2a: zeroclaw_config::multi_agent::AgentA2aConfig {
                    published: true,
                    exposed_skills: Vec::new(),
                },
                ..Default::default()
            };
            config.agents.insert("maker".to_string(), agent);
            let mut state = test_state(config);
            state.pairing = Arc::new(PairingGuard::new(true, &[TOKEN.to_string()]));
            state
        }

        async fn status_of(
            router: axum::Router,
            req: axum::http::Request<axum::body::Body>,
        ) -> StatusCode {
            router.oneshot(req).await.expect("router response").status()
        }

        #[tokio::test]
        async fn task_endpoint_rejects_unauthenticated_request() {
            let router = crate::a2a::a2a_task_route().with_state(paired_state());
            let req = axum::http::Request::builder()
                .method("POST")
                .uri("/a2a/maker")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    r#"{"jsonrpc":"2.0","id":1,"method":"message/send","params":{"message":{"parts":[{"kind":"text","text":"hi"}]}}}"#,
                ))
                .unwrap();
            assert_eq!(status_of(router, req).await, StatusCode::UNAUTHORIZED);
        }

        #[tokio::test]
        async fn catalog_card_serves_unauthenticated_request() {
            let router = crate::a2a::a2a_routes().with_state(paired_state());
            let req = axum::http::Request::builder()
                .method("GET")
                .uri("/.well-known/agents-card.json")
                .body(axum::body::Body::empty())
                .unwrap();
            assert_eq!(status_of(router, req).await, StatusCode::OK);
        }

        #[tokio::test]
        async fn alias_card_serves_unauthenticated_request() {
            let router = crate::a2a::a2a_routes().with_state(paired_state());
            let req = axum::http::Request::builder()
                .method("GET")
                .uri("/a2a/maker/.well-known/agent-card.json")
                .body(axum::body::Body::empty())
                .unwrap();
            assert_eq!(status_of(router, req).await, StatusCode::OK);
        }

        #[tokio::test]
        async fn alias_card_serves_with_valid_token() {
            let router = crate::a2a::a2a_routes().with_state(paired_state());
            let req = axum::http::Request::builder()
                .method("GET")
                .uri("/a2a/maker/.well-known/agent-card.json")
                .header("authorization", format!("Bearer {TOKEN}"))
                .body(axum::body::Body::empty())
                .unwrap();
            assert_eq!(status_of(router, req).await, StatusCode::OK);
        }
    }

    // ── resolve_session_key ───────────────────────────────────

    #[test]
    fn resolve_session_key_preserves_gw_prefixed_key() {
        // Step 1: gw_ prefix → identity (sanitized).
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;
        assert_eq!(resolve_session_key("gw_foo", backend).unwrap(), "gw_foo");
        assert_eq!(
            resolve_session_key("gw_test-session", backend).unwrap(),
            "gw_test-session"
        );
        assert_eq!(
            resolve_session_key("gw_foo.bar", backend).unwrap(),
            "gw_foo_bar"
        );
    }

    #[test]
    fn resolve_session_key_backend_based_disambiguation() {
        // Steps 2-4: consult backend to disambiguate.
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        // No sessions exist: bare ID → gw_ prefix (step 4).
        assert_eq!(
            resolve_session_key("my-session", backend).unwrap(),
            "gw_my-session"
        );
        assert_eq!(
            resolve_session_key("550e8400-e29b-41d4-a716-446655440000", backend).unwrap(),
            "gw_550e8400-e29b-41d4-a716-446655440000"
        );

        // Create a gw_ session: gw_{id} lookup should find it (step 2).
        store
            .append("gw_my-session", &ChatMessage::user("hello"))
            .unwrap();
        assert_eq!(
            resolve_session_key("my-session", backend).unwrap(),
            "gw_my-session"
        );

        // Create a channel key: bare lookup should find it (step 3).
        store
            .append("discord_clamps_user123", &ChatMessage::user("hi"))
            .unwrap();
        assert_eq!(
            resolve_session_key("discord_clamps_user123", backend).unwrap(),
            "discord_clamps_user123"
        );
    }

    #[test]
    fn resolve_session_key_sanitizes_input() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;
        // Spaces and dots are sanitized; no session exists → gw_ prefix (step 4).
        assert_eq!(
            resolve_session_key("my session", backend).unwrap(),
            "gw_my_session"
        );
    }

    // ── DELETE handler session_queue serialization tests ─────────────

    #[tokio::test]
    async fn delete_waits_for_session_queue_guard() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let backend: Arc<dyn SessionBackend> = Arc::new(SessionStore::new(tmp.path()).unwrap());
        backend
            .append(
                "gw_del_block",
                &zeroclaw_providers::ChatMessage::user("hello"),
            )
            .unwrap();
        let state = test_state_with_session_backend(config, backend.clone());
        let session_guard = state.session_queue.acquire("gw_del_block").await.unwrap();

        let response_fut = handle_api_session_delete(
            State(state),
            HeaderMap::new(),
            Path("del_block".to_string()),
        );
        tokio::pin!(response_fut);

        // DELETE must block behind the active guard
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut response_fut)
                .await
                .is_err(),
            "DELETE should block behind active session queue guard"
        );
        // Session still exists while guard is held
        assert!(backend.session_exists("gw_del_block"));

        drop(session_guard);
        let response = tokio::time::timeout(Duration::from_secs(1), response_fut)
            .await
            .expect("DELETE should complete after guard released")
            .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        // Session is now deleted
        assert!(!backend.session_exists("gw_del_block"));
    }

    #[tokio::test]
    async fn delete_returns_429_when_queue_full() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let backend: Arc<dyn SessionBackend> = Arc::new(SessionStore::new(tmp.path()).unwrap());
        let mut state = test_state_with_session_backend(config, backend.clone());
        // max_queue_depth=1: acquire the single slot, DELETE gets QueueFull
        state.session_queue = std::sync::Arc::new(
            zeroclaw_infra::session_queue::SessionActorQueue::new(1, 30, 600),
        );
        backend
            .append("gw_del_full", &zeroclaw_providers::ChatMessage::user("hi"))
            .unwrap();
        // Fill the only slot — second acquire will be QueueFull
        let _guard = state.session_queue.acquire("gw_del_full").await.unwrap();

        let response =
            handle_api_session_delete(State(state), HeaderMap::new(), Path("del_full".to_string()))
                .await
                .into_response();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        // Session was NOT deleted (queue guard not acquired)
        assert!(backend.session_exists("gw_del_full"));
    }

    #[tokio::test]
    async fn delete_returns_409_when_queue_times_out() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let backend: Arc<dyn SessionBackend> = Arc::new(SessionStore::new(tmp.path()).unwrap());
        let mut state = test_state_with_session_backend(config, backend.clone());
        // lock_timeout_secs=0: acquire times out immediately
        state.session_queue = std::sync::Arc::new(
            zeroclaw_infra::session_queue::SessionActorQueue::new(8, 0, 600),
        );
        backend
            .append(
                "gw_del_timeout",
                &zeroclaw_providers::ChatMessage::user("hi"),
            )
            .unwrap();
        // Hold the guard so the DELETE's acquire will time out
        let _guard = state.session_queue.acquire("gw_del_timeout").await.unwrap();

        let response = handle_api_session_delete(
            State(state),
            HeaderMap::new(),
            Path("del_timeout".to_string()),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::CONFLICT);
        // Session was NOT deleted (timeout → fail-closed)
        assert!(backend.session_exists("gw_del_timeout"));
    }

    #[tokio::test]
    async fn resolve_session_key_ambiguous_returns_409_on_delete() {
        let tmp = tempfile::TempDir::new().unwrap();
        let backend: Arc<dyn SessionBackend> = Arc::new(SessionStore::new(tmp.path()).unwrap());
        backend
            .append(
                "gw_discord_clamps_user123",
                &ChatMessage::user("gateway-msg"),
            )
            .unwrap();
        backend
            .append("discord_clamps_user123", &ChatMessage::user("channel-msg"))
            .unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let state = test_state_with_session_backend(config, backend.clone());

        // DELETE with ambiguous key → 409
        let response = handle_api_session_delete(
            State(state),
            HeaderMap::new(),
            Path("discord_clamps_user123".to_string()),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);

        // Both sessions still exist (nothing was deleted)
        assert!(backend.session_exists("gw_discord_clamps_user123"));
        assert!(backend.session_exists("discord_clamps_user123"));

        // Using the full gw_ key (identity escape hatch) succeeds
        let state2 = test_state_with_session_backend(
            zeroclaw_config::schema::Config {
                data_dir: tmp.path().join("workspace"),
                config_path: tmp.path().join("config.toml"),
                ..Default::default()
            },
            backend.clone(),
        );
        let response2 = handle_api_session_delete(
            State(state2),
            HeaderMap::new(),
            Path("gw_discord_clamps_user123".to_string()),
        )
        .await
        .into_response();
        assert_eq!(response2.status(), StatusCode::OK);
        assert!(!backend.session_exists("gw_discord_clamps_user123"));
        assert!(backend.session_exists("discord_clamps_user123"));
    }
}
