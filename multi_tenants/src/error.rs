use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("rate limited")]
    RateLimited,
    #[error("internal: {0}")]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".into()),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg.clone()),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, msg.clone()),
            AppError::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "rate limited".into()),
            AppError::Internal(e) => {
                tracing::error!("internal error: {:?}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".into(),
                )
            }
        };
        (status, axum::Json(serde_json::json!({ "error": message }))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn body_json(resp: Response) -> serde_json::Value {
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn not_found_returns_404() {
        let err = AppError::NotFound("tenant xyz".into());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let json = body_json(resp).await;
        assert_eq!(json["error"], "tenant xyz");
    }

    #[tokio::test]
    async fn unauthorized_returns_401() {
        let resp = AppError::Unauthorized.into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rate_limited_returns_429() {
        let resp = AppError::RateLimited.into_response();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn internal_hides_details() {
        let err = AppError::Internal(anyhow::anyhow!("db connection pool exhausted"));
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let json = body_json(resp).await;
        // must not leak internal detail
        assert_eq!(json["error"], "internal server error");
    }
}
