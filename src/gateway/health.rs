//! Health, readiness, and metrics endpoints for production observability.
//!
//! - `GET /health` — liveness check (process alive, basic sanity)
//! - `GET /ready`  — readiness check (all configured components healthy)
//! - `GET /metrics` — Prometheus text exposition format (handled in gateway/mod.rs)

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::Serialize;
use std::collections::BTreeMap;

use super::AppState;

/// Minimal liveness response — intentionally excludes internal details
/// (uptime, version, component checks) to avoid information disclosure
/// to unauthenticated callers.
#[derive(Debug, Serialize)]
pub struct LivenessResponse {
    pub status: &'static str,
}

/// Structured readiness response with component checks.
#[derive(Debug, Serialize)]
pub struct ReadinessResponse {
    pub status: &'static str,
    pub checks: BTreeMap<String, CheckResult>,
}

/// Individual check result.
#[derive(Debug, Serialize)]
pub struct CheckResult {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `GET /health` — liveness probe.
///
/// Returns 200 with `{"status": "healthy"}` if the process is alive.
/// Intentionally minimal to avoid exposing internal details (uptime,
/// config, version) to unauthenticated callers.
pub async fn handle_liveness(State(_state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(LivenessResponse { status: "healthy" }),
    )
}

/// `GET /ready` — readiness probe.
///
/// Returns 200 with `"ok"` when all configured providers/channels are
/// initialized and healthy. Returns 503 with `"unhealthy"` when any
/// required component is not ready.
///
/// Checks the health registry for real component status rather than
/// synthesizing fake healthy states.
pub async fn handle_readiness(State(_state): State<AppState>) -> impl IntoResponse {
    let mut checks = BTreeMap::new();
    let mut all_ok = true;

    // Check component health from the health registry.
    // Components register themselves on startup (gateway, channels, etc.).
    // If no components have registered yet, that itself indicates the
    // service is still starting up.
    let snapshot = crate::health::snapshot();

    if snapshot.components.is_empty() {
        // No components registered yet — service is still initializing.
        all_ok = false;
        checks.insert(
            "startup".to_string(),
            CheckResult {
                status: "degraded",
                message: Some("no components registered yet".to_string()),
            },
        );
    }

    for (name, component) in &snapshot.components {
        let status = match component.status.as_str() {
            "ok" => "ok",
            "error" => {
                all_ok = false;
                "unhealthy"
            }
            "starting" => {
                all_ok = false;
                "degraded"
            }
            _ => {
                all_ok = false;
                "degraded"
            }
        };
        checks.insert(
            name.clone(),
            CheckResult {
                status,
                message: component.last_error.clone(),
            },
        );
    }

    let overall = if all_ok { "ok" } else { "unhealthy" };
    let status_code = if all_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    let resp = ReadinessResponse {
        status: overall,
        checks,
    };

    (status_code, Json(resp))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liveness_response_is_minimal() {
        let resp = LivenessResponse { status: "healthy" };
        let json = serde_json::to_value(&resp).expect("should serialize");
        assert_eq!(json["status"], "healthy");
        // Must not contain internal details
        assert!(json.get("uptime_secs").is_none());
        assert!(json.get("checks").is_none());
        assert!(json.get("version").is_none());
    }

    #[test]
    fn readiness_response_serializes_correctly() {
        let mut checks = BTreeMap::new();
        checks.insert(
            "gateway".to_string(),
            CheckResult {
                status: "ok",
                message: None,
            },
        );

        let resp = ReadinessResponse {
            status: "ok",
            checks,
        };

        let json = serde_json::to_value(&resp).expect("should serialize");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["checks"]["gateway"]["status"], "ok");
    }

    #[test]
    fn readiness_response_unhealthy_status() {
        let mut checks = BTreeMap::new();
        checks.insert(
            "provider".to_string(),
            CheckResult {
                status: "unhealthy",
                message: Some("timeout".to_string()),
            },
        );

        let resp = ReadinessResponse {
            status: "unhealthy",
            checks,
        };

        let json = serde_json::to_value(&resp).expect("should serialize");
        assert_eq!(json["status"], "unhealthy");
        assert_eq!(json["checks"]["provider"]["status"], "unhealthy");
    }

    #[test]
    fn check_result_skips_none_message() {
        let check = CheckResult {
            status: "ok",
            message: None,
        };
        let json = serde_json::to_value(&check).expect("should serialize");
        assert!(!json.as_object().unwrap().contains_key("message"));
    }
}
