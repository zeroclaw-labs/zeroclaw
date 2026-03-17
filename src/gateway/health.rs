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

/// Liveness response including pairing state and runtime snapshot.
///
/// Preserves backward compatibility with existing consumers that expect
/// `paired`, `require_pairing`, and `runtime` fields alongside `status`.
#[derive(Debug, Serialize)]
pub struct LivenessResponse {
    pub status: &'static str,
    pub paired: bool,
    pub require_pairing: bool,
    pub runtime: serde_json::Value,
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
/// Returns 200 with `{"status": "ok"}` plus pairing state and runtime
/// health snapshot. No secrets are leaked — only boolean flags and the
/// component health registry.
pub async fn handle_liveness(State(state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(LivenessResponse {
            status: "ok",
            paired: state.pairing.is_paired(),
            require_pairing: state.pairing.require_pairing(),
            runtime: crate::health::snapshot_json(),
        }),
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
    let mut has_error = false;
    let mut has_degraded = false;

    // Check component health from the health registry.
    // Components register themselves on startup (gateway, channels, etc.).
    // If no components have registered yet, that itself indicates the
    // service is still starting up.
    let snapshot = crate::health::snapshot();

    if snapshot.components.is_empty() {
        // No components registered yet — service is still initializing.
        has_degraded = true;
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
                has_error = true;
                "unhealthy"
            }
            _ => {
                has_degraded = true;
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

    let overall = if has_error {
        "unhealthy"
    } else if has_degraded {
        "degraded"
    } else {
        "ok"
    };
    let status_code = if has_error || has_degraded {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
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
    fn liveness_response_includes_required_fields() {
        let resp = LivenessResponse {
            status: "ok",
            paired: true,
            require_pairing: false,
            runtime: serde_json::json!({"components": {}}),
        };
        let json = serde_json::to_value(&resp).expect("should serialize");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["paired"], true);
        assert_eq!(json["require_pairing"], false);
        assert!(json.get("runtime").is_some());
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
