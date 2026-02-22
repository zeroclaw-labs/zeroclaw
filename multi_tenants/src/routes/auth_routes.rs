use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::auth::otp;
use crate::error::AppError;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct OtpRequest {
    pub email: String,
}

#[derive(Serialize)]
pub struct OtpResponse {
    pub message: String,
}

/// POST /auth/otp/request
pub async fn request_otp(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OtpRequest>,
) -> Result<Json<OtpResponse>, AppError> {
    // 1. Rate limit check
    if !state.otp_limiter.check_and_record(&body.email) {
        return Err(AppError::RateLimited);
    }

    // 2. Check user exists
    let user_id: Option<String> = state.db.read(|conn| {
        match conn.query_row(
            "SELECT id FROM users WHERE email = ?1",
            [&body.email],
            |row| row.get(0),
        ) {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    })?;

    // Anti-enumeration: always return the same response regardless of user existence
    let Some(user_id) = user_id else {
        return Ok(Json(OtpResponse {
            message: "if account exists, OTP sent".into(),
        }));
    };

    // 3. Generate OTP
    let code = otp::generate_otp();
    let salt = otp::generate_salt();
    let hash = otp::hash_otp(&salt, &code);
    let otp_id = uuid::Uuid::new_v4().to_string();
    let expires_at = (chrono::Utc::now() + chrono::Duration::minutes(10))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();

    // 4. Invalidate old OTPs + store new one
    state.db.write(|conn| {
        conn.execute(
            "UPDATE otp_tokens SET used = 1 WHERE user_id = ?1 AND used = 0",
            [&user_id],
        )?;
        conn.execute(
            "INSERT INTO otp_tokens (id, user_id, hash, salt, expires_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![otp_id, user_id, hash, salt, expires_at],
        )?;
        Ok(())
    })?;

    // 5. Send OTP via email (fallback to console log if SMTP not configured)
    if let Some(ref email_svc) = state.email {
        if let Err(e) = email_svc.send_otp(&body.email, &code).await {
            tracing::error!("Failed to send OTP email to {}: {}", body.email, e);
        }
    } else {
        tracing::info!("OTP sent to {}", body.email);
    }

    // 6. Audit
    crate::audit::log(&state.db, "otp_requested", "user", &user_id, None, None)?;

    Ok(Json(OtpResponse {
        message: "if account exists, OTP sent".into(),
    }))
}

#[derive(Deserialize)]
pub struct OtpVerify {
    pub email: String,
    pub code: String,
}

#[derive(Serialize)]
pub struct TokenResponse {
    pub token: String,
    pub expires_in: i64,
}

/// POST /auth/otp/verify
pub async fn verify_otp(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OtpVerify>,
) -> Result<Json<TokenResponse>, AppError> {
    // 1. Find user
    let (user_id, user_email): (String, String) = state
        .db
        .read(|conn| {
            conn.query_row(
                "SELECT id, email FROM users WHERE email = ?1",
                [&body.email],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| anyhow::anyhow!("invalid credentials"))
        })
        .map_err(|_| AppError::Unauthorized)?;

    // 2. Find latest unused, non-expired OTP that has not exceeded attempt limit
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let otp_row: (String, String, String) = state
        .db
        .read(|conn| {
            conn.query_row(
                "SELECT id, hash, salt FROM otp_tokens
                 WHERE user_id = ?1 AND used = 0 AND expires_at > ?2 AND attempts < 5
                 ORDER BY created_at DESC LIMIT 1",
                rusqlite::params![user_id, now],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|_| anyhow::anyhow!("no valid OTP"))
        })
        .map_err(|_| AppError::Unauthorized)?;

    // 3. Constant-time verify; increment attempts on failure before returning error
    if !otp::verify_otp(&otp_row.2, &body.code, &otp_row.1) {
        let otp_id_fail = otp_row.0.clone();
        let _ = state.db.write(|conn| {
            conn.execute(
                "UPDATE otp_tokens SET attempts = attempts + 1 WHERE id = ?1",
                [&otp_id_fail],
            )?;
            Ok(())
        });
        return Err(AppError::Unauthorized);
    }

    // 4. Mark OTP as used
    let otp_id = otp_row.0.clone();
    state.db.write(|conn| {
        conn.execute("UPDATE otp_tokens SET used = 1 WHERE id = ?1", [&otp_id])?;
        Ok(())
    })?;

    // 5. Load tenant roles
    let tenant_roles = state.db.read(|conn| {
        let mut stmt = conn.prepare("SELECT tenant_id, role FROM members WHERE user_id = ?1")?;
        let roles = stmt
            .query_map([&user_id], |row| {
                Ok(crate::auth::jwt::TenantRole {
                    tenant_id: row.get(0)?,
                    role: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(roles)
    })?;

    // 6. Issue JWT
    let token = state.jwt.issue(&user_id, &user_email, tenant_roles)?;

    // 7. Audit
    crate::audit::log(&state.db, "login_success", "user", &user_id, None, None)?;

    Ok(Json(TokenResponse {
        token,
        expires_in: 86400,
    }))
}

/// POST /auth/logout — revokes the caller's token so subsequent requests are rejected.
pub async fn logout(
    State(state): State<Arc<AppState>>,
    user: crate::auth::middleware::AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    state.jwt.revoke(&user.token);
    crate::audit::log(&state.db, "logout", "user", &user.claims.sub, None, None)?;
    Ok(Json(serde_json::json!({ "logged_out": true })))
}

/// GET /auth/me -- returns current authenticated user info
pub async fn me(
    State(state): State<Arc<AppState>>,
    user: crate::auth::middleware::AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    // Load tenant roles from DB (fresh, not from JWT cache)
    let user_id = user.claims.sub.clone();
    let tenant_roles: Vec<serde_json::Value> = state.db.read(|conn| {
        let mut stmt = conn.prepare(
            "SELECT m.tenant_id, t.name, t.slug, m.role FROM members m JOIN tenants t ON t.id = m.tenant_id WHERE m.user_id = ?1"
        )?;
        let rows = stmt.query_map([&user_id], |row| {
            Ok(serde_json::json!({
                "tenant_id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "slug": row.get::<_, String>(2)?,
                "role": row.get::<_, String>(3)?,
            }))
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;

    Ok(Json(serde_json::json!({
        "id": user.claims.sub,
        "email": user.claims.email,
        "is_super_admin": user.is_super_admin,
        "tenant_roles": tenant_roles,
    })))
}
