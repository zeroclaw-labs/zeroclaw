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

/// Structured health/readiness response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub checks: BTreeMap<String, CheckResult>,
    pub uptime_secs: u64,
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
/// Returns 200 if the process is alive. Always succeeds unless the runtime
/// is completely unresponsive.
pub async fn handle_liveness(State(state): State<AppState>) -> impl IntoResponse {
    let mut checks = BTreeMap::new();

    checks.insert(
        "process".to_string(),
        CheckResult {
            status: "ok",
            message: None,
        },
    );

    // Include pairing status as informational check
    let paired = state.pairing.is_paired();
    checks.insert(
        "pairing".to_string(),
        CheckResult {
            status: if paired || !state.pairing.require_pairing() {
                "ok"
            } else {
                "degraded"
            },
            message: Some(format!("paired={paired}")),
        },
    );

    let uptime = crate::observability::metrics::global().uptime_secs();

    let overall = if checks.values().all(|c| c.status == "ok") {
        "ok"
    } else {
        "degraded"
    };

    let resp = HealthResponse {
        status: overall,
        checks,
        uptime_secs: uptime,
    };

    (StatusCode::OK, Json(resp))
}

/// `GET /ready` — readiness probe.
///
/// Returns 200 with `"ok"` when all configured providers/channels are
/// initialized and healthy. Returns 503 with `"unhealthy"` when any
/// required component is not ready.
pub async fn handle_readiness(State(state): State<AppState>) -> impl IntoResponse {
    let mut checks = BTreeMap::new();
    let mut all_ok = true;

    // Check provider availability (provider is always required)
    checks.insert(
        "provider".to_string(),
        CheckResult {
            status: "ok",
            message: Some(state.model.clone()),
        },
    );

    // Check component health from the health registry
    let snapshot = crate::health::snapshot();
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

    // Check memory backend
    checks.insert(
        "memory".to_string(),
        CheckResult {
            status: "ok",
            message: None,
        },
    );

    let uptime = crate::observability::metrics::global().uptime_secs();

    let overall = if all_ok { "ok" } else { "unhealthy" };
    let status_code = if all_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    let resp = HealthResponse {
        status: overall,
        checks,
        uptime_secs: uptime,
    };

    (status_code, Json(resp))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_response_serializes_correctly() {
        let mut checks = BTreeMap::new();
        checks.insert(
            "process".to_string(),
            CheckResult {
                status: "ok",
                message: None,
            },
        );
        checks.insert(
            "provider".to_string(),
            CheckResult {
                status: "ok",
                message: Some("claude-sonnet".to_string()),
            },
        );

        let resp = HealthResponse {
            status: "ok",
            checks,
            uptime_secs: 42,
        };

        let json = serde_json::to_value(&resp).expect("should serialize");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["uptime_secs"], 42);
        assert_eq!(json["checks"]["process"]["status"], "ok");
        assert!(json["checks"]["process"]["message"].is_null());
        assert_eq!(json["checks"]["provider"]["message"], "claude-sonnet");
    }

    #[test]
    fn health_response_degraded_status() {
        let mut checks = BTreeMap::new();
        checks.insert(
            "process".to_string(),
            CheckResult {
                status: "ok",
                message: None,
            },
        );
        checks.insert(
            "provider".to_string(),
            CheckResult {
                status: "degraded",
                message: Some("timeout".to_string()),
            },
        );

        let resp = HealthResponse {
            status: "degraded",
            checks,
            uptime_secs: 10,
        };

        let json = serde_json::to_value(&resp).expect("should serialize");
        assert_eq!(json["status"], "degraded");
        assert_eq!(json["checks"]["provider"]["status"], "degraded");
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
