use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::auth::middleware::AuthUser;
use crate::error::AppError;
use crate::state::AppState;

#[derive(Serialize)]
pub struct UserResponse {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
    pub is_super_admin: bool,
    pub created_at: String,
}

#[derive(Deserialize)]
pub struct CreateUser {
    pub email: String,
    pub name: Option<String>,
}

/// GET /users — super_admin only
pub async fn list_users(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
) -> Result<Json<Vec<UserResponse>>, AppError> {
    crate::rbac::require_super_admin(&user)?;

    let users = state.db.read(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, email, name, is_super_admin, created_at FROM users ORDER BY created_at DESC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(UserResponse {
                    id: row.get(0)?,
                    email: row.get(1)?,
                    name: row.get(2)?,
                    is_super_admin: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;

    Ok(Json(users))
}

/// POST /users — super_admin only
pub async fn create_user(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Json(body): Json<CreateUser>,
) -> Result<Json<UserResponse>, AppError> {
    crate::rbac::require_super_admin(&user)?;

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    state.db.write(|conn| {
        conn.execute(
            "INSERT INTO users (id, email, name) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, body.email, body.name],
        )
        .map_err(|e| {
            if e.to_string().contains("UNIQUE") {
                return anyhow::Error::from(AppError::Conflict("email already exists".into()));
            }
            anyhow::anyhow!("db error: {}", e)
        })?;
        Ok(())
    })?;

    crate::audit::log(
        &state.db,
        "user_created",
        "user",
        &id,
        Some(&user.claims.sub),
        None,
    )?;

    Ok(Json(UserResponse {
        id,
        email: body.email,
        name: body.name,
        is_super_admin: false,
        created_at: now,
    }))
}

#[derive(Deserialize)]
pub struct UpdateUser {
    pub name: Option<String>,
    pub is_super_admin: Option<bool>,
}

/// PATCH /users/{id} — super_admin only
pub async fn update_user(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(user_id): Path<String>,
    Json(body): Json<UpdateUser>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::rbac::require_super_admin(&user)?;

    // Prevent removing own super_admin status
    if user_id == user.claims.sub && body.is_super_admin == Some(false) {
        return Err(AppError::BadRequest(
            "cannot revoke your own super_admin status".into(),
        ));
    }

    let mut sets: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let mut idx = 1usize;

    if let Some(ref name) = body.name {
        sets.push(format!("name = ?{}", idx));
        params.push(Box::new(name.clone()));
        idx += 1;
    }
    if let Some(is_admin) = body.is_super_admin {
        sets.push(format!("is_super_admin = ?{}", idx));
        params.push(Box::new(is_admin as i32));
        idx += 1;
    }

    if sets.is_empty() {
        return Err(AppError::BadRequest("no fields to update".into()));
    }

    params.push(Box::new(user_id.clone()));
    let sql = format!("UPDATE users SET {} WHERE id = ?{}", sets.join(", "), idx);
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let updated = state.db.write(|conn| {
        let count = conn.execute(&sql, param_refs.as_slice())?;
        Ok(count)
    })?;

    if updated == 0 {
        return Err(AppError::NotFound("user not found".into()));
    }

    crate::audit::log(
        &state.db,
        "user_updated",
        "user",
        &user_id,
        Some(&user.claims.sub),
        None,
    )?;

    Ok(Json(serde_json::json!({ "updated": true })))
}

/// DELETE /users/{id} — super_admin only
pub async fn delete_user(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(user_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::rbac::require_super_admin(&user)?;

    // Prevent self-deletion
    if user_id == user.claims.sub {
        return Err(AppError::BadRequest("cannot delete yourself".into()));
    }

    let deleted = state.db.write(|conn| {
        let count = conn.execute("DELETE FROM users WHERE id = ?1", [&user_id])?;
        Ok(count)
    })?;

    if deleted == 0 {
        return Err(AppError::NotFound("user not found".into()));
    }

    crate::audit::log(
        &state.db,
        "user_deleted",
        "user",
        &user_id,
        Some(&user.claims.sub),
        None,
    )?;

    Ok(Json(serde_json::json!({ "deleted": true })))
}
