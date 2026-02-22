use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::auth::middleware::AuthUser;
use crate::error::AppError;
use crate::rbac::Role;
use crate::state::AppState;
use crate::tenant::provisioner::{CreateDraftInput, CreateTenantInput};

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateTenant {
    pub name: String,
    /// Super-admin override for slug. When absent, slug is auto-generated from name.
    #[serde(default)]
    pub custom_slug: Option<String>,
    pub plan: String,
    /// Optional for two-phase creation. If absent, creates a draft tenant.
    pub api_key: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    #[serde(default = "default_autonomy")]
    pub autonomy_level: String,
    pub system_prompt: Option<String>,
}

fn default_temperature() -> f64 {
    0.7
}

fn default_autonomy() -> String {
    "supervised".to_string()
}

#[derive(Serialize)]
pub struct TenantResponse {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub status: String,
    pub plan: String,
    pub port: u16,
    pub created_at: String,
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

// ---------------------------------------------------------------------------
// Slug validation (used for custom_slug override)
// ---------------------------------------------------------------------------

/// Validate a tenant slug: 3-30 lowercase alphanumeric + hyphens,
/// no leading or trailing hyphens.
pub fn is_valid_slug(slug: &str) -> bool {
    let len = slug.len();
    if !(3..=30).contains(&len) {
        return false;
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /tenants
/// Super-admin sees all tenants. Others see only tenants they are members of.
pub async fn list_tenants(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<TenantResponse>>, AppError> {
    let limit = query.limit.unwrap_or(50) as i64;
    let offset = query.offset.unwrap_or(0) as i64;

    let tenants = if user.is_super_admin {
        state.db.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, slug, status, plan, port, created_at
                 FROM tenants
                 ORDER BY created_at DESC
                 LIMIT ?1 OFFSET ?2",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![limit, offset], |row| {
                    Ok(TenantResponse {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        slug: row.get(2)?,
                        status: row.get(3)?,
                        plan: row.get(4)?,
                        port: row.get::<_, i64>(5)? as u16,
                        created_at: row.get(6)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?
    } else {
        // Build list of tenant IDs the user belongs to from JWT claims
        let tenant_ids: Vec<String> = user
            .claims
            .tenant_roles
            .iter()
            .map(|tr| tr.tenant_id.clone())
            .collect();

        if tenant_ids.is_empty() {
            return Ok(Json(vec![]));
        }

        state.db.read(|conn| {
            // Build a parameterized IN clause dynamically
            let placeholders: String = tenant_ids
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", i + 3))
                .collect::<Vec<_>>()
                .join(", ");

            let sql = format!(
                "SELECT t.id, t.name, t.slug, t.status, t.plan, t.port, t.created_at
                 FROM tenants t
                 WHERE t.id IN ({})
                 ORDER BY t.created_at DESC
                 LIMIT ?1 OFFSET ?2",
                placeholders
            );

            let mut stmt = conn.prepare(&sql)?;

            // Bind limit, offset, then tenant IDs
            let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(limit), Box::new(offset)];
            for tid in &tenant_ids {
                params.push(Box::new(tid.clone()));
            }

            let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

            let rows = stmt
                .query_map(param_refs.as_slice(), |row| {
                    Ok(TenantResponse {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        slug: row.get(2)?,
                        status: row.get(3)?,
                        plan: row.get(4)?,
                        port: row.get::<_, i64>(5)? as u16,
                        created_at: row.get(6)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?
    };

    Ok(Json(tenants))
}

/// POST /tenants — super_admin only
///
/// Two modes:
/// - **Full** (legacy): supply `api_key` + `provider` + `model` → provisions immediately
/// - **Draft** (two-phase): omit `api_key` → creates DB record with status='draft',
///   caller then PATCH /config + POST /deploy
pub async fn create_tenant(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Json(body): Json<CreateTenant>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::rbac::require_super_admin(&user)?;

    // Validate custom_slug if provided
    if let Some(ref custom_slug) = body.custom_slug {
        if !is_valid_slug(custom_slug) {
            return Err(AppError::BadRequest(
                "invalid custom_slug: must be 3-30 lowercase alphanumeric + hyphens, \
                 no leading/trailing hyphens"
                    .into(),
            ));
        }
    }

    let domain_suffix = state.config.domain.as_deref().unwrap_or("localhost");

    // Branch: draft vs full creation
    let is_draft = body.api_key.is_none();

    if is_draft {
        // Two-phase: create DB record only
        let draft_input = CreateDraftInput {
            name: body.name.clone(),
            custom_slug: body.custom_slug.clone(),
            plan: body.plan.clone(),
        };

        let (tenant_id, slug) = state
            .provisioner
            .create_draft(&state.db, &state.vault, draft_input)
            .map_err(|e: anyhow::Error| {
                if e.to_string().contains("already exists") {
                    AppError::Conflict(e.to_string())
                } else {
                    AppError::Internal(e)
                }
            })?;

        crate::audit::log(
            &state.db,
            "tenant_created",
            "tenant",
            &tenant_id,
            Some(&user.claims.sub),
            Some(&format!("draft slug={}", slug)),
        )?;

        let subdomain = format!("{}.{}", slug, domain_suffix);

        return Ok(Json(serde_json::json!({
            "id": tenant_id,
            "slug": slug,
            "subdomain": subdomain,
            "status": "draft"
        })));
    }

    // Full creation (legacy): api_key is present
    let input = CreateTenantInput {
        name: body.name.clone(),
        custom_slug: body.custom_slug.clone(),
        plan: body.plan.clone(),
        api_key: body.api_key.clone().unwrap_or_default(),
        provider: body.provider.clone().unwrap_or_default(),
        model: body.model.clone().unwrap_or_default(),
        temperature: body.temperature,
        autonomy_level: body.autonomy_level.clone(),
        system_prompt: body.system_prompt.clone(),
    };

    let proxy_ref = state.proxy.as_ref();

    let (tenant_id, slug) = state
        .provisioner
        .create_tenant(&state.db, &state.docker, &state.vault, proxy_ref, input)
        .await
        .map_err(|e: anyhow::Error| {
            if e.to_string().contains("already exists") {
                AppError::Conflict(e.to_string())
            } else {
                AppError::Internal(e)
            }
        })?;

    crate::audit::log(
        &state.db,
        "tenant_created",
        "tenant",
        &tenant_id,
        Some(&user.claims.sub),
        Some(&slug),
    )?;

    let subdomain = format!("{}.{}", slug, domain_suffix);

    Ok(Json(serde_json::json!({
        "id": tenant_id,
        "slug": slug,
        "subdomain": subdomain,
        "status": "provisioning"
    })))
}

/// DELETE /tenants/{id} — Owner role required
pub async fn delete_tenant(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(tenant_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Owner, &state.db)?;

    let proxy_ref = state.proxy.as_ref();

    state
        .provisioner
        .delete_tenant(&tenant_id, &state.db, &state.docker, proxy_ref)
        .await
        .map_err(|e: anyhow::Error| {
            if e.to_string().contains("not found") {
                AppError::NotFound(format!("tenant {} not found", tenant_id))
            } else {
                AppError::Internal(e)
            }
        })?;

    crate::audit::log(
        &state.db,
        "tenant_deleted",
        "tenant",
        &tenant_id,
        Some(&user.claims.sub),
        None,
    )?;

    Ok(Json(serde_json::json!({ "deleted": true })))
}

/// POST /tenants/{id}/restart — Manager role required
pub async fn restart_tenant(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(tenant_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Manager, &state.db)?;

    state
        .provisioner
        .restart_tenant(
            &tenant_id,
            &state.db,
            &state.docker,
            &state.vault,
            state.proxy.as_ref(),
        )
        .await
        .map_err(|e: anyhow::Error| {
            if e.to_string().contains("not found") {
                AppError::NotFound(format!("tenant {} not found", tenant_id))
            } else {
                AppError::Internal(e)
            }
        })?;

    crate::audit::log(
        &state.db,
        "tenant_restarted",
        "tenant",
        &tenant_id,
        Some(&user.claims.sub),
        None,
    )?;

    Ok(Json(serde_json::json!({ "restarted": true })))
}

/// POST /tenants/{id}/stop — Manager role required
pub async fn stop_tenant(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(tenant_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Manager, &state.db)?;

    // Fetch slug for docker
    let slug: String = state
        .db
        .read(|conn| {
            conn.query_row(
                "SELECT slug FROM tenants WHERE id = ?1",
                rusqlite::params![tenant_id],
                |row| row.get(0),
            )
            .map_err(|e| anyhow::anyhow!("tenant not found: {}", e))
        })
        .map_err(|e| AppError::NotFound(e.to_string()))?;

    state
        .docker
        .stop_container(&slug)
        .map_err(AppError::Internal)?;

    state.db.write(|conn| {
        conn.execute(
            "UPDATE tenants SET status = 'stopped' WHERE id = ?1",
            rusqlite::params![tenant_id],
        )?;
        Ok(())
    })?;

    crate::audit::log(
        &state.db,
        "tenant_stopped",
        "tenant",
        &tenant_id,
        Some(&user.claims.sub),
        None,
    )?;

    Ok(Json(serde_json::json!({ "stopped": true })))
}

/// GET /tenants/{id}/logs — Contributor role required
pub async fn tenant_logs(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(tenant_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Contributor, &state.db)?;

    let slug: String = state
        .db
        .read(|conn| {
            conn.query_row(
                "SELECT slug FROM tenants WHERE id = ?1",
                rusqlite::params![tenant_id],
                |row| row.get(0),
            )
            .map_err(|e| anyhow::anyhow!("tenant not found: {}", e))
        })
        .map_err(|e| AppError::NotFound(e.to_string()))?;

    let logs = state.docker.logs(&slug, 100).map_err(AppError::Internal)?;

    Ok(Json(serde_json::json!({ "logs": logs })))
}

/// GET /tenants/{id}/config — Manager role required
pub async fn get_tenant_config(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(tenant_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Manager, &state.db)?;

    let config = state.db.read(|conn| {
        conn.query_row(
            "SELECT api_key_enc, provider, model, temperature, autonomy_level, system_prompt, extra_json
             FROM tenant_configs WHERE tenant_id = ?1",
            [&tenant_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .map_err(|e| anyhow::anyhow!("config not found: {}", e))
    })?;

    let (api_key_enc, provider, model, temperature, autonomy_level, system_prompt, extra_json) =
        config;

    // Decrypt API key for display (mask all but last 4 chars)
    let api_key_display = match state.vault.decrypt(&api_key_enc) {
        Ok(key) => {
            if key.len() > 4 {
                format!("{}...{}", "*".repeat(key.len() - 4), &key[key.len() - 4..])
            } else {
                "****".to_string()
            }
        }
        Err(_) => "****".to_string(),
    };

    // Parse and mask tool_settings from extra_json
    let tool_settings = extra_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.get("tool_settings").cloned())
        .map(|ts| crate::tenant::config_render::mask_tool_secrets(&ts));

    Ok(Json(serde_json::json!({
        "provider": provider,
        "model": model,
        "temperature": temperature,
        "autonomy_level": autonomy_level,
        "system_prompt": system_prompt,
        "api_key_masked": api_key_display,
        "tool_settings": tool_settings,
    })))
}

#[derive(Deserialize)]
pub struct UpdateTenantConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub temperature: Option<f64>,
    pub autonomy_level: Option<String>,
    pub system_prompt: Option<String>,
    /// New plaintext key to re-encrypt
    pub api_key: Option<String>,
    /// Tool/skill configuration JSON
    pub tool_settings: Option<serde_json::Value>,
}

/// PATCH /tenants/{id}/config — Manager role required
pub async fn update_tenant_config(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(tenant_id): Path<String>,
    Json(body): Json<UpdateTenantConfig>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Manager, &state.db)?;

    // Build SET clauses dynamically
    let mut sets: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let mut idx = 1usize;

    if let Some(ref provider) = body.provider {
        sets.push(format!("provider = ?{}", idx));
        params.push(Box::new(provider.clone()));
        idx += 1;
    }
    if let Some(ref model) = body.model {
        sets.push(format!("model = ?{}", idx));
        params.push(Box::new(model.clone()));
        idx += 1;
    }
    if let Some(temp) = body.temperature {
        sets.push(format!("temperature = ?{}", idx));
        params.push(Box::new(temp));
        idx += 1;
    }
    if let Some(ref autonomy) = body.autonomy_level {
        sets.push(format!("autonomy_level = ?{}", idx));
        params.push(Box::new(autonomy.clone()));
        idx += 1;
    }
    if let Some(ref prompt) = body.system_prompt {
        sets.push(format!("system_prompt = ?{}", idx));
        params.push(Box::new(prompt.clone()));
        idx += 1;
    }
    if let Some(ref api_key) = body.api_key {
        let encrypted = state.vault.encrypt(api_key).map_err(AppError::Internal)?;
        sets.push(format!("api_key_enc = ?{}", idx));
        params.push(Box::new(encrypted));
        idx += 1;
    }

    if sets.is_empty() && body.tool_settings.is_none() {
        return Err(AppError::BadRequest("no fields to update".into()));
    }

    // Handle tool_settings separately — stored in extra_json column
    if let Some(ref ts) = body.tool_settings {
        // Read current extra_json
        let current_extra: Option<String> = state
            .db
            .read(|conn| {
                conn.query_row(
                    "SELECT extra_json FROM tenant_configs WHERE tenant_id = ?1",
                    [&tenant_id],
                    |row| row.get(0),
                )
                .map_err(|e| anyhow::anyhow!("{}", e))
            })
            .unwrap_or(None);

        let mut extra: serde_json::Map<String, serde_json::Value> = current_extra
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();

        // Encrypt secrets before storing
        let encrypted_ts = crate::tenant::config_render::encrypt_tool_secrets(ts, &state.vault)
            .map_err(AppError::Internal)?;

        extra.insert("tool_settings".to_string(), encrypted_ts);
        let extra_str = serde_json::to_string(&serde_json::Value::Object(extra))
            .map_err(|e| AppError::Internal(anyhow::anyhow!("JSON serialize failed: {}", e)))?;

        // Separate UPDATE for extra_json column
        state.db.write(|conn| {
            conn.execute(
                "UPDATE tenant_configs SET extra_json = ?1 WHERE tenant_id = ?2",
                rusqlite::params![extra_str, tenant_id],
            )?;
            Ok(())
        })?;
    }

    // Only run main UPDATE if there are non-tool_settings fields to update
    let updated = if !sets.is_empty() {
        // Add tenant_id as last param
        let tid_idx = idx;
        params.push(Box::new(tenant_id.clone()));

        let sql = format!(
            "UPDATE tenant_configs SET {} WHERE tenant_id = ?{}",
            sets.join(", "),
            tid_idx
        );

        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

        state.db.write(|conn| {
            let count = conn.execute(&sql, param_refs.as_slice())?;
            Ok(count)
        })?
    } else {
        // tool_settings-only update: verify tenant config row exists
        state.db.read(|conn| {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM tenant_configs WHERE tenant_id = ?1",
                [&tenant_id],
                |row| row.get(0),
            )?;
            Ok(count as usize)
        })?
    };

    if updated == 0 {
        return Err(AppError::NotFound("tenant config not found".into()));
    }

    crate::audit::log(
        &state.db,
        "tenant_config_updated",
        "tenant_config",
        &tenant_id,
        Some(&user.claims.sub),
        None,
    )?;

    // Sync config into running container (rewrite config.toml + recreate container).
    // Runs in background — Caddy route already exists from initial provisioning.
    let state_clone = state.clone();
    let tid = tenant_id.clone();
    tokio::spawn(async move {
        if let Err(e) = state_clone
            .provisioner
            .sync_and_restart(
                &tid,
                &state_clone.db,
                &state_clone.docker,
                &state_clone.vault,
                None,
            )
            .await
        {
            tracing::error!("config sync failed for {}: {}", tid, e);
        }
    });

    Ok(Json(serde_json::json!({ "updated": true })))
}

/// POST /tenants/{id}/deploy — Manager role required
///
/// Triggers container provisioning for a draft tenant.
/// The tenant must have status='draft' and a valid config (provider + api_key set via PATCH /config).
pub async fn deploy_tenant(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(tenant_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Manager, &state.db)?;

    let proxy_ref = state.proxy.as_ref();

    // Deploy runs in foreground so the client can await completion
    state
        .provisioner
        .deploy_tenant(
            &tenant_id,
            &state.db,
            &state.docker,
            &state.vault,
            proxy_ref,
        )
        .await
        .map_err(|e: anyhow::Error| {
            let msg = e.to_string();
            if msg.contains("not 'draft'") || msg.contains("incomplete") {
                AppError::BadRequest(msg)
            } else if msg.contains("not found") {
                AppError::NotFound(msg)
            } else {
                AppError::Internal(e)
            }
        })?;

    crate::audit::log(
        &state.db,
        "tenant_deployed",
        "tenant",
        &tenant_id,
        Some(&user.claims.sub),
        None,
    )?;

    // Read final status
    let status: String = state
        .db
        .read(|conn| {
            conn.query_row(
                "SELECT status FROM tenants WHERE id = ?1",
                rusqlite::params![tenant_id],
                |row| row.get(0),
            )
            .map_err(|e| anyhow::anyhow!("{}", e))
        })
        .unwrap_or_else(|_| "unknown".to_string());

    Ok(Json(serde_json::json!({
        "deployed": true,
        "status": status
    })))
}

/// GET /tenants/{id}/status — Viewer role required
///
/// Lightweight status check: returns tenant status + container running state.
pub async fn get_tenant_status(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(tenant_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Viewer, &state.db)?;

    let (status, slug): (String, String) = state
        .db
        .read(|conn| {
            conn.query_row(
                "SELECT status, slug FROM tenants WHERE id = ?1",
                rusqlite::params![tenant_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| anyhow::anyhow!("tenant not found: {}", e))
        })
        .map_err(|e| AppError::NotFound(e.to_string()))?;

    let container_running = state.docker.is_running(&slug).unwrap_or(false);

    Ok(Json(serde_json::json!({
        "status": status,
        "container_running": container_running
    })))
}

#[derive(Deserialize)]
pub struct TestProviderBody {
    pub provider: String,
    pub api_key: String,
    pub model: Option<String>,
}

/// POST /tenants/{id}/provider/test — Manager role required
///
/// Tests whether the given provider API key is valid by making a
/// lightweight read-only API call (list models or similar).
pub async fn test_provider(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(tenant_id): Path<String>,
    Json(body): Json<TestProviderBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Manager, &state.db)?;

    let (url, auth_header) = match body.provider.as_str() {
        "openai" => (
            "https://api.openai.com/v1/models".to_string(),
            format!("Bearer {}", body.api_key),
        ),
        "anthropic" => (
            "https://api.anthropic.com/v1/models".to_string(),
            body.api_key.clone(), // uses x-api-key header
        ),
        "google" => (
            format!(
                "https://generativelanguage.googleapis.com/v1/models?key={}",
                body.api_key
            ),
            String::new(), // key in URL
        ),
        "groq" => (
            "https://api.groq.com/openai/v1/models".to_string(),
            format!("Bearer {}", body.api_key),
        ),
        "deepseek" => (
            "https://api.deepseek.com/v1/models".to_string(),
            format!("Bearer {}", body.api_key),
        ),
        "openrouter" => (
            "https://openrouter.ai/api/v1/models".to_string(),
            format!("Bearer {}", body.api_key),
        ),
        _ => {
            // For unknown providers, skip connection test — just validate non-empty key
            if body.api_key.is_empty() {
                return Err(AppError::BadRequest("api_key is empty".into()));
            }
            return Ok(Json(serde_json::json!({
                "success": true,
                "message": "API key format accepted (provider-specific test not available)"
            })));
        }
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| anyhow::anyhow!("http client error: {}", e))?;

    let mut req = client.get(&url);
    if body.provider == "anthropic" {
        req = req
            .header("x-api-key", &body.api_key)
            .header("anthropic-version", "2023-06-01");
    } else if !auth_header.is_empty() {
        req = req.header("Authorization", &auth_header);
    }

    match req.send().await {
        Ok(resp) => {
            if resp.status().is_success() {
                Ok(Json(serde_json::json!({
                    "success": true,
                    "message": "API key is valid"
                })))
            } else {
                let status = resp.status().as_u16();
                let err_body = resp.text().await.unwrap_or_default();
                let msg = if status == 401 || status == 403 {
                    "Invalid API key or insufficient permissions"
                } else {
                    "Provider returned an error"
                };
                Ok(Json(serde_json::json!({
                    "success": false,
                    "message": msg,
                    "status_code": status,
                    "detail": err_body.chars().take(200).collect::<String>()
                })))
            }
        }
        Err(e) => Ok(Json(serde_json::json!({
            "success": false,
            "message": format!("Connection failed: {}", e)
        }))),
    }
}

/// POST /tenants/{id}/exec — Manager role required
///
/// Executes a whitelisted command inside the tenant's container.
/// Only specific safe commands are allowed (e.g., channel bind operations).
pub async fn exec_in_tenant(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(tenant_id): Path<String>,
    Json(body): Json<ExecBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Manager, &state.db)?;

    // Whitelist of allowed commands
    let allowed_prefixes = [
        "channel bind-telegram",
        "channel bind-discord",
        "channel bind-slack",
        "channel bind-whatsapp",
    ];

    let cmd_trimmed = body.command.trim();
    if !allowed_prefixes
        .iter()
        .any(|prefix| cmd_trimmed.starts_with(prefix))
    {
        return Err(AppError::BadRequest(format!(
            "command not allowed. Permitted: {}",
            allowed_prefixes.join(", ")
        )));
    }

    // Validate no shell injection characters
    if cmd_trimmed.contains(';')
        || cmd_trimmed.contains('|')
        || cmd_trimmed.contains('&')
        || cmd_trimmed.contains('`')
        || cmd_trimmed.contains('$')
    {
        return Err(AppError::BadRequest(
            "command contains invalid characters".into(),
        ));
    }

    // Get slug
    let slug: String = state
        .db
        .read(|conn| {
            conn.query_row(
                "SELECT slug FROM tenants WHERE id = ?1",
                rusqlite::params![tenant_id],
                |row| row.get(0),
            )
            .map_err(|e| anyhow::anyhow!("tenant not found: {}", e))
        })
        .map_err(|e| AppError::NotFound(e.to_string()))?;

    // Build the command: zeroclaw <subcommand> <args>
    let parts: Vec<&str> = cmd_trimmed.split_whitespace().collect();
    let mut exec_args = vec!["zeroclaw"];
    exec_args.extend_from_slice(&parts);

    let output = state
        .docker
        .exec_in_container(&slug, &exec_args, 15)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("exec failed: {}", e)))?;

    crate::audit::log(
        &state.db,
        "tenant_exec",
        "tenant",
        &tenant_id,
        Some(&user.claims.sub),
        Some(cmd_trimmed),
    )?;

    Ok(Json(serde_json::json!({
        "success": true,
        "output": output
    })))
}

#[derive(Deserialize)]
pub struct ExecBody {
    pub command: String,
}

/// GET /tenants/{id}/pairing-code — Viewer role required
///
/// Returns the stored pairing code for the tenant's ZeroClaw web dashboard.
/// The code is a one-time 6-digit number the end user enters in the browser.
pub async fn get_pairing_code(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(tenant_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Viewer, &state.db)?;

    let (pairing_code, status): (Option<String>, String) = state
        .db
        .read(|conn| {
            conn.query_row(
                "SELECT pairing_code, status FROM tenants WHERE id = ?1",
                rusqlite::params![tenant_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| anyhow::anyhow!("tenant not found: {}", e))
        })
        .map_err(|e| AppError::NotFound(e.to_string()))?;

    Ok(Json(serde_json::json!({
        "pairing_code": pairing_code,
        "status": status,
    })))
}

/// POST /tenants/{id}/reset-pairing — Manager role required
///
/// Clears existing paired tokens from config.toml (removes the
/// `paired_tokens` line), then restarts the container so ZeroClaw
/// generates a fresh pairing code.
pub async fn reset_pairing(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(tenant_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Manager, &state.db)?;

    let (slug, uid): (String, u32) = state
        .db
        .read(|conn| {
            conn.query_row(
                "SELECT slug, uid FROM tenants WHERE id = ?1",
                rusqlite::params![tenant_id],
                |row| Ok((row.get(0)?, row.get::<_, i64>(1).map(|u| u as u32)?)),
            )
            .map_err(|e| anyhow::anyhow!("tenant not found: {}", e))
        })
        .map_err(|e| AppError::NotFound(e.to_string()))?;

    // Clear paired_tokens from the on-disk config.toml so ZeroClaw
    // generates a fresh pairing code on next startup.
    let data_dir = &state.provisioner.data_dir;
    let config_path = format!("{}/{}/zeroclaw-home/config.toml", data_dir, slug);
    match std::fs::read_to_string(&config_path) {
        Ok(content) => {
            // Remove any paired_tokens line from [gateway] section
            let cleaned: String = content
                .lines()
                .filter(|line| !line.trim_start().starts_with("paired_tokens"))
                .collect::<Vec<_>>()
                .join("\n");
            if let Err(e) = std::fs::write(&config_path, &cleaned) {
                tracing::warn!("failed to write cleaned config for {}: {}", slug, e);
            }
            // Fix ownership back to container UID
            crate::tenant::filesystem::ensure_tenant_ownership(data_dir, &slug, uid)
                .map_err(AppError::Internal)?;
        }
        Err(e) => {
            tracing::warn!("failed to read config for {}: {}", slug, e);
        }
    }

    // Clear stored pairing code
    crate::tenant::pairing::clear_pairing_code(&state.db, &tenant_id)
        .map_err(AppError::Internal)?;

    // Restart container — provisioner will read + store new code after health check
    state
        .provisioner
        .restart_tenant(
            &tenant_id,
            &state.db,
            &state.docker,
            &state.vault,
            state.proxy.as_ref(),
        )
        .await
        .map_err(AppError::Internal)?;

    // Read the new pairing code from DB (provisioner stored it)
    let new_code: Option<String> = state
        .db
        .read(|conn| {
            conn.query_row(
                "SELECT pairing_code FROM tenants WHERE id = ?1",
                rusqlite::params![tenant_id],
                |row| row.get(0),
            )
            .map_err(|e| anyhow::anyhow!("{}", e))
        })
        .unwrap_or(None);

    crate::audit::log(
        &state.db,
        "tenant_pairing_reset",
        "tenant",
        &tenant_id,
        Some(&user.claims.sub),
        None,
    )?;

    Ok(Json(serde_json::json!({
        "reset": true,
        "pairing_code": new_code,
    })))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_slugs() {
        assert!(is_valid_slug("abc"));
        assert!(is_valid_slug("my-tenant"));
        assert!(is_valid_slug("zeroclaw-prod"));
        assert!(is_valid_slug("tenant123"));
        assert!(is_valid_slug("a1b"));
        assert!(is_valid_slug("abc-def-ghi"));
        // Exactly 30 chars
        assert!(is_valid_slug("abcdefghijklmnopqrstuvwxyz1234"));
    }

    #[test]
    fn test_invalid_slugs_too_short() {
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug("a"));
        assert!(!is_valid_slug("ab"));
    }

    #[test]
    fn test_invalid_slugs_too_long() {
        // 31 chars
        assert!(!is_valid_slug("abcdefghijklmnopqrstuvwxyz12345"));
    }

    #[test]
    fn test_invalid_slugs_uppercase() {
        assert!(!is_valid_slug("MyTenant"));
        assert!(!is_valid_slug("TENANT"));
        assert!(!is_valid_slug("tenantA"));
    }

    #[test]
    fn test_invalid_slugs_special_chars() {
        assert!(!is_valid_slug("tenant_one"));
        assert!(!is_valid_slug("tenant.one"));
        assert!(!is_valid_slug("tenant one"));
        assert!(!is_valid_slug("tenant@one"));
        assert!(!is_valid_slug("tenant/one"));
    }

    #[test]
    fn test_invalid_slugs_leading_trailing_hyphens() {
        assert!(!is_valid_slug("-tenant"));
        assert!(!is_valid_slug("tenant-"));
        assert!(!is_valid_slug("-tenant-"));
    }
}
