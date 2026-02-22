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

#[derive(Deserialize)]
pub struct AddMember {
    pub email: String,
    pub role: String,
}

#[derive(Deserialize)]
pub struct UpdateMemberRole {
    pub role: String,
}

#[derive(Serialize)]
pub struct MemberResponse {
    pub id: String,
    pub user_id: String,
    pub email: String,
    pub name: Option<String>,
    pub role: String,
    pub created_at: String,
}

/// Resolve plan max_members from plan string.
fn plan_max_members(state: &AppState, plan: &str) -> u32 {
    match plan {
        "pro" => state.config.plans.pro.max_members,
        _ => state.config.plans.free.max_members,
    }
}

/// GET /tenants/{tenant_id}/members — Viewer role required.
pub async fn list_members(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(tenant_id): Path<String>,
) -> Result<Json<Vec<MemberResponse>>, AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Viewer, &state.db)?;

    let members = state.db.read(|conn| {
        let mut stmt = conn.prepare(
            "SELECT m.id, m.user_id, u.email, u.name, m.role, m.created_at
             FROM members m
             JOIN users u ON u.id = m.user_id
             WHERE m.tenant_id = ?1
             ORDER BY m.created_at ASC",
        )?;
        let rows = stmt
            .query_map([&tenant_id], |row| {
                Ok(MemberResponse {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    email: row.get(2)?,
                    name: row.get(3)?,
                    role: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;

    Ok(Json(members))
}

/// POST /tenants/{tenant_id}/members — Owner role required.
pub async fn add_member(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(tenant_id): Path<String>,
    Json(body): Json<AddMember>,
) -> Result<(StatusCode, Json<MemberResponse>), AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Owner, &state.db)?;

    // Validate role
    if Role::parse_role(&body.role).is_none() {
        return Err(AppError::BadRequest("invalid role".into()));
    }

    // Load tenant plan
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

    let max_members = plan_max_members(&state, &plan);

    // Count current members
    let current_count: u32 = state.db.read(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM members WHERE tenant_id = ?1",
            [&tenant_id],
            |row| row.get(0),
        )
        .map_err(|e| anyhow::anyhow!("count error: {}", e))
    })?;

    if current_count >= max_members {
        return Err(AppError::Forbidden(format!(
            "member limit reached for plan {} (max {})",
            plan, max_members
        )));
    }

    // Find user by email
    let user_row: Option<(String, String, Option<String>)> = state.db.read(|conn| {
        match conn.query_row(
            "SELECT id, email, name FROM users WHERE email = ?1",
            [&body.email],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ) {
            Ok(r) => Ok(Some(r)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    })?;

    let Some((target_user_id, target_email, target_name)) = user_row else {
        return Err(AppError::NotFound(format!(
            "no user found with email {}",
            body.email
        )));
    };

    let member_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    // Insert member; handle UNIQUE(user_id, tenant_id) constraint
    state.db.write(|conn| {
        conn.execute(
            "INSERT INTO members (id, user_id, tenant_id, role) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![member_id, target_user_id, tenant_id, body.role],
        )
        .map_err(|e| {
            if e.to_string().contains("UNIQUE") {
                return anyhow::Error::from(AppError::Conflict(
                    "user is already a member of this tenant".into(),
                ));
            }
            anyhow::anyhow!("db error: {}", e)
        })?;
        Ok(())
    })?;

    crate::audit::log(
        &state.db,
        "member_added",
        "member",
        &member_id,
        Some(&user.claims.sub),
        Some(&format!("user_id={} role={}", target_user_id, body.role)),
    )?;

    Ok((
        StatusCode::CREATED,
        Json(MemberResponse {
            id: member_id,
            user_id: target_user_id,
            email: target_email,
            name: target_name,
            role: body.role,
            created_at: now,
        }),
    ))
}

/// PATCH /tenants/{tenant_id}/members/{member_id} — Owner role required.
pub async fn update_member_role(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((tenant_id, member_id)): Path<(String, String)>,
    Json(body): Json<UpdateMemberRole>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Owner, &state.db)?;

    // Validate role
    if Role::parse_role(&body.role).is_none() {
        return Err(AppError::BadRequest("invalid role".into()));
    }

    // Only super_admin may promote to owner
    if body.role == "owner" && !user.is_super_admin {
        return Err(AppError::Forbidden(
            "only super_admin can promote to owner".into(),
        ));
    }

    // Prevent demoting the last owner
    if body.role != "owner" {
        let current_role: String = state
            .db
            .read(|conn| {
                conn.query_row(
                    "SELECT role FROM members WHERE id = ?1 AND tenant_id = ?2",
                    rusqlite::params![member_id, tenant_id],
                    |row| row.get(0),
                )
                .map_err(|e| anyhow::anyhow!("member lookup: {}", e))
            })
            .map_err(|_| AppError::NotFound("member not found".into()))?;

        if current_role == "owner" {
            let owner_count: i64 = state.db.read(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM members WHERE tenant_id = ?1 AND role = 'owner'",
                    [&tenant_id],
                    |row| row.get(0),
                )
                .map_err(|e| anyhow::anyhow!("owner count: {}", e))
            })?;

            if owner_count <= 1 {
                return Err(AppError::BadRequest("cannot demote the last owner".into()));
            }
        }
    }

    let updated = state.db.write(|conn| {
        let count = conn.execute(
            "UPDATE members SET role = ?1 WHERE id = ?2 AND tenant_id = ?3",
            rusqlite::params![body.role, member_id, tenant_id],
        )?;
        Ok(count)
    })?;

    if updated == 0 {
        return Err(AppError::NotFound("member not found".into()));
    }

    crate::audit::log(
        &state.db,
        "member_role_updated",
        "member",
        &member_id,
        Some(&user.claims.sub),
        Some(&format!("new_role={}", body.role)),
    )?;

    Ok(Json(
        serde_json::json!({ "updated": true, "role": body.role }),
    ))
}

/// DELETE /tenants/{tenant_id}/members/{member_id} — Owner role required.
pub async fn remove_member(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((tenant_id, member_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::rbac::require_tenant_role(&user, &tenant_id, Role::Owner, &state.db)?;

    // Resolve the member's user_id to prevent self-removal
    let member_user_id: Option<String> = state.db.read(|conn| {
        match conn.query_row(
            "SELECT user_id FROM members WHERE id = ?1 AND tenant_id = ?2",
            rusqlite::params![member_id, tenant_id],
            |row| row.get(0),
        ) {
            Ok(uid) => Ok(Some(uid)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    })?;

    let Some(member_user_id) = member_user_id else {
        return Err(AppError::NotFound("member not found".into()));
    };

    // Prevent self-removal
    if member_user_id == user.claims.sub {
        return Err(AppError::BadRequest("cannot remove yourself".into()));
    }

    state.db.write(|conn| {
        conn.execute(
            "DELETE FROM members WHERE id = ?1 AND tenant_id = ?2",
            rusqlite::params![member_id, tenant_id],
        )?;
        Ok(())
    })?;

    crate::audit::log(
        &state.db,
        "member_removed",
        "member",
        &member_id,
        Some(&user.claims.sub),
        None,
    )?;

    Ok(Json(serde_json::json!({ "deleted": true })))
}
