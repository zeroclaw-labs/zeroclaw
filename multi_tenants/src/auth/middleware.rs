use crate::auth::jwt::Claims;
use crate::error::AppError;
use crate::state::AppState;
use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use rusqlite::OptionalExtension;
use std::sync::Arc;

/// Authenticated user extracted from a valid JWT in the Authorization header.
/// `is_super_admin` is loaded from the database, not from the JWT payload.
/// `token` is the raw Bearer token string, stored for use in logout/revocation.
pub struct AuthUser {
    pub claims: Claims,
    pub is_super_admin: bool,
    pub token: String,
}

impl FromRequestParts<Arc<AppState>> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        // Extract Bearer token from Authorization header
        let auth_header = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(AppError::Unauthorized)?;

        let token = auth_header
            .strip_prefix("Bearer ")
            .ok_or(AppError::Unauthorized)?;

        // Check revocation before verifying claims
        if state.jwt.is_revoked(token) {
            return Err(AppError::Unauthorized);
        }

        // Verify JWT
        let claims = state
            .jwt
            .verify(token)
            .map_err(|_| AppError::Unauthorized)?;

        // Load is_super_admin from DB (not from JWT)
        let user_id = claims.sub.clone();
        let is_super_admin = state
            .db
            .read(|conn| {
                let result: Option<bool> = conn
                    .query_row(
                        "SELECT is_super_admin FROM users WHERE id = ?1",
                        rusqlite::params![user_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                Ok(result.unwrap_or(false))
            })
            .map_err(AppError::Internal)?;

        Ok(AuthUser {
            claims,
            is_super_admin,
            token: token.to_string(),
        })
    }
}
