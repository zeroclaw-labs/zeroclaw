use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::auth::middleware::AuthUser;
use crate::error::AppError;
use crate::rbac::Role;
use crate::state::AppState;

/// Channel kinds accepted by the platform.
pub const VALID_KINDS: &[&str] = &[
    "telegram",
    "discord",
    "slack",
    "mattermost",
    "webhook",
    "whatsapp",
    "email",
    "irc",
    "matrix",
    "signal",
    "lark",
    "dingtalk",
    "qq",
];

#[derive(Deserialize)]
pub struct CreateChannel {
    pub kind: String,
    pub config: serde_json::Value,
}

/// Channel list item — no config exposed.
#[derive(Serialize)]
pub struct ChannelResponse {
    pub id: String,
    pub tenant_id: String,
    pub kind: String,
    pub enabled: bool,
    pub created_at: String,
}

/// Channel detail — includes decrypted config.
#[derive(Serialize)]
pub struct ChannelDetailResponse {
    pub id: String,
    pub tenant_id: String,
    pub kind: String,
    pub config: serde_json::Value,
    pub enabled: bool,
    pub created_at: String,
}

/// Resolve plan limits from `tenants.plan` string.
fn plan_max_channels(state: &AppState, plan: &str) -> u32 {
    match plan {
        "pro" => state.config.plans.pro.max_channels,
        _ => state.config.plans.free.max_channels,
    }
}

/// GET /tenants/{tenant_id}/channels — Viewer role required.
pub async fn list_channels(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(tenant_id): Path<String>,
) -> Result<Json<Vec<ChannelResponse>>, AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Viewer, &state.db)?;

    let channels = state.db.read(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, tenant_id, kind, enabled, created_at
             FROM channels
             WHERE tenant_id = ?1
             ORDER BY created_at ASC",
        )?;
        let rows = stmt
            .query_map([&tenant_id], |row| {
                let enabled_int: i64 = row.get(3)?;
                Ok(ChannelResponse {
                    id: row.get(0)?,
                    tenant_id: row.get(1)?,
                    kind: row.get(2)?,
                    enabled: enabled_int != 0,
                    created_at: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;

    Ok(Json(channels))
}

/// POST /tenants/{tenant_id}/channels — Manager role required.
pub async fn create_channel(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(tenant_id): Path<String>,
    Json(body): Json<CreateChannel>,
) -> Result<(StatusCode, Json<ChannelResponse>), AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Manager, &state.db)?;

    // Validate channel kind
    if !VALID_KINDS.contains(&body.kind.as_str()) {
        return Err(AppError::BadRequest(format!(
            "invalid channel kind: {}",
            body.kind
        )));
    }

    // Enforce config payload size limit (4 KB)
    let config_json = serde_json::to_string(&body.config)
        .map_err(|e| anyhow::anyhow!("config serialize error: {}", e))?;
    if config_json.len() > 4096 {
        return Err(AppError::BadRequest(
            "channel config too large (max 4KB)".into(),
        ));
    }

    // Load tenant plan to enforce channel limit
    let (plan,): (String,) = state
        .db
        .read(|conn| {
            conn.query_row(
                "SELECT plan FROM tenants WHERE id = ?1",
                [&tenant_id],
                |row| Ok((row.get(0)?,)),
            )
            .map_err(|_| anyhow::anyhow!("tenant not found"))
        })
        .map_err(|_| AppError::NotFound("tenant not found".into()))?;

    let max_channels = plan_max_channels(&state, &plan);

    // Encrypt config before the write transaction
    let config_enc = state.vault.encrypt(&config_json)?;

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    // Atomically check current channel count and insert — prevents race conditions
    // that could exceed plan limits under concurrent requests.
    state.db.write(|conn| {
        let current_count: u32 = conn.query_row(
            "SELECT COUNT(*) FROM channels WHERE tenant_id = ?1",
            [&tenant_id],
            |row| row.get(0),
        )?;

        if current_count >= max_channels {
            return Err(anyhow::Error::from(AppError::Forbidden(format!(
                "channel limit reached for plan {} (max {})",
                plan, max_channels
            ))));
        }

        conn.execute(
            "INSERT INTO channels (id, tenant_id, kind, config_enc, enabled)
             VALUES (?1, ?2, ?3, ?4, 1)",
            rusqlite::params![id, tenant_id, body.kind, config_enc],
        )?;
        Ok(())
    })?;

    crate::audit::log(
        &state.db,
        "channel_created",
        "channel",
        &id,
        Some(&user.claims.sub),
        Some(&format!("kind={}", body.kind)),
    )?;

    // Sync config into container (rewrite config.toml + recreate with channels)
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
            tracing::error!("channel sync failed for tenant {}: {}", tid, e);
        }
    });

    Ok((
        StatusCode::CREATED,
        Json(ChannelResponse {
            id,
            tenant_id,
            kind: body.kind,
            enabled: true,
            created_at: now,
        }),
    ))
}

/// GET /tenants/{tenant_id}/channels/{channel_id} — Manager role required.
/// Returns decrypted config.
pub async fn get_channel(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((tenant_id, channel_id)): Path<(String, String)>,
) -> Result<Json<ChannelDetailResponse>, AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Manager, &state.db)?;

    let row: (String, String, String, i64, String) = state
        .db
        .read(|conn| {
            conn.query_row(
                "SELECT id, tenant_id, kind, config_enc, enabled, created_at
                 FROM channels
                 WHERE id = ?1 AND tenant_id = ?2",
                rusqlite::params![channel_id, tenant_id],
                |row| {
                    Ok((
                        row.get::<_, String>(2)?, // kind
                        row.get::<_, String>(3)?, // config_enc
                        row.get::<_, String>(0)?, // id
                        row.get::<_, i64>(4)?,    // enabled
                        row.get::<_, String>(5)?, // created_at
                    ))
                },
            )
            .map_err(|_| anyhow::anyhow!("channel not found"))
        })
        .map_err(|_| AppError::NotFound("channel not found".into()))?;

    let (kind, config_enc, id, enabled_int, created_at) = row;

    // Decrypt config
    let config_json = state.vault.decrypt(&config_enc)?;
    let config: serde_json::Value = serde_json::from_str(&config_json)
        .map_err(|e| anyhow::anyhow!("config deserialize error: {}", e))?;

    Ok(Json(ChannelDetailResponse {
        id,
        tenant_id,
        kind,
        config,
        enabled: enabled_int != 0,
        created_at,
    }))
}

#[derive(Deserialize)]
pub struct UpdateChannel {
    pub config: Option<serde_json::Value>,
    pub enabled: Option<bool>,
}

/// PATCH /tenants/{tenant_id}/channels/{channel_id} — Manager role required.
///
/// Updates channel config (deep-merge with existing) and/or enabled state.
pub async fn update_channel(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((tenant_id, channel_id)): Path<(String, String)>,
    Json(body): Json<UpdateChannel>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Manager, &state.db)?;

    if body.config.is_none() && body.enabled.is_none() {
        return Err(AppError::BadRequest("no fields to update".into()));
    }

    // Read existing channel
    let (existing_enc, _existing_enabled): (String, i64) = state
        .db
        .read(|conn| {
            conn.query_row(
                "SELECT config_enc, enabled FROM channels WHERE id = ?1 AND tenant_id = ?2",
                rusqlite::params![channel_id, tenant_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| anyhow::anyhow!("channel not found"))
        })
        .map_err(|_| AppError::NotFound("channel not found".into()))?;

    // Merge config if provided
    let new_config_enc = if let Some(ref new_config) = body.config {
        // Enforce size limit
        let new_json = serde_json::to_string(new_config)
            .map_err(|e| anyhow::anyhow!("config serialize error: {}", e))?;
        if new_json.len() > 4096 {
            return Err(AppError::BadRequest(
                "channel config too large (max 4KB)".into(),
            ));
        }

        // Decrypt old → merge → re-encrypt
        let old_json = state.vault.decrypt(&existing_enc)?;
        let mut old_val: serde_json::Value = serde_json::from_str(&old_json)
            .map_err(|e| anyhow::anyhow!("old config parse error: {}", e))?;

        // Deep merge: new values override old
        if let (Some(old_obj), Some(new_obj)) = (old_val.as_object_mut(), new_config.as_object()) {
            for (k, v) in new_obj {
                old_obj.insert(k.clone(), v.clone());
            }
        } else {
            old_val = new_config.clone();
        }

        let merged_json = serde_json::to_string(&old_val)
            .map_err(|e| anyhow::anyhow!("merge serialize error: {}", e))?;
        Some(state.vault.encrypt(&merged_json)?)
    } else {
        None
    };

    let new_enabled = body.enabled.map(|e| if e { 1i64 } else { 0i64 });

    // Build UPDATE
    let mut sets: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let mut idx = 1usize;

    if let Some(ref enc) = new_config_enc {
        sets.push(format!("config_enc = ?{}", idx));
        params.push(Box::new(enc.clone()));
        idx += 1;
    }
    if let Some(enabled) = new_enabled {
        sets.push(format!("enabled = ?{}", idx));
        params.push(Box::new(enabled));
        idx += 1;
    }

    params.push(Box::new(channel_id.clone()));
    let cid_idx = idx;
    idx += 1;
    params.push(Box::new(tenant_id.clone()));
    let tid_idx = idx;

    let sql = format!(
        "UPDATE channels SET {} WHERE id = ?{} AND tenant_id = ?{}",
        sets.join(", "),
        cid_idx,
        tid_idx
    );
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let updated = state.db.write(|conn| {
        let count = conn.execute(&sql, param_refs.as_slice())?;
        Ok(count)
    })?;

    if updated == 0 {
        return Err(AppError::NotFound("channel not found".into()));
    }

    crate::audit::log(
        &state.db,
        "channel_updated",
        "channel",
        &channel_id,
        Some(&user.claims.sub),
        None,
    )?;

    // Sync config into container
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
            tracing::error!("channel update sync failed for tenant {}: {}", tid, e);
        }
    });

    Ok(Json(serde_json::json!({ "updated": true })))
}

/// DELETE /tenants/{tenant_id}/channels/{channel_id} — Manager role required.
pub async fn delete_channel(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((tenant_id, channel_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Manager, &state.db)?;

    let deleted = state.db.write(|conn| {
        let count = conn.execute(
            "DELETE FROM channels WHERE id = ?1 AND tenant_id = ?2",
            rusqlite::params![channel_id, tenant_id],
        )?;
        Ok(count)
    })?;

    if deleted == 0 {
        return Err(AppError::NotFound("channel not found".into()));
    }

    crate::audit::log(
        &state.db,
        "channel_deleted",
        "channel",
        &channel_id,
        Some(&user.claims.sub),
        None,
    )?;

    // Sync config into container (remove channel from config.toml + restart)
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
            tracing::error!("channel sync failed for tenant {}: {}", tid, e);
        }
    });

    Ok(Json(serde_json::json!({ "deleted": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_channel_kinds() {
        let expected = [
            "telegram",
            "discord",
            "slack",
            "mattermost",
            "webhook",
            "whatsapp",
            "email",
            "irc",
            "matrix",
            "signal",
            "lark",
            "dingtalk",
            "qq",
        ];
        for kind in &expected {
            assert!(
                VALID_KINDS.contains(kind),
                "expected kind '{}' to be valid",
                kind
            );
        }
        assert_eq!(VALID_KINDS.len(), expected.len());
    }

    #[test]
    fn test_invalid_channel_kind_rejected() {
        // Kinds that must NOT be in VALID_KINDS
        let invalid = ["xmpp", "sms", "twitter", "rss", "custom", "", "Telegram"];
        for kind in &invalid {
            assert!(
                !VALID_KINDS.contains(kind),
                "kind '{}' should be invalid",
                kind
            );
        }
    }
}
