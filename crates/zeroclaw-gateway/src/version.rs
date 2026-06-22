//! Version-check and restart-classification helpers for the dashboard's
//! upgrade affordance (RFC: In-app upgrade with optional supervised restart).
//!
//! Phase 1 (read-only): `GET /api/version/check` reports whether a newer
//! release exists, plus release notes, by shelling out to
//! `zeroclaw update --check --json` — keeping a single source of truth for
//! update logic. Results are cached for an hour to stay well under GitHub's
//! unauthenticated rate limit.
//!
//! Restart classification is advisory only here: it tells the dashboard which
//! restart command to show after an upgrade. The gateway never restarts itself
//! in Phase 1.

use super::AppState;
use super::api::require_auth;
use anyhow::Context;
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::{IntoResponse, Json},
};
use serde::Deserialize;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long a successful version check is reused before re-querying GitHub.
const CHECK_CACHE_TTL: Duration = Duration::from_secs(3600);
/// Upper bound on the `zeroclaw update --check` subprocess.
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);

// ── Restart classification (advisory) ────────────────────────────

/// Whether a clean process exit will be relaunched by a supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartMode {
    /// A supervisor (systemd/launchd) will relaunch us on exit.
    Supervised,
    /// No supervisor will relaunch us — the operator must restart manually.
    Manual,
}

impl RestartMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RestartMode::Supervised => "supervised",
            RestartMode::Manual => "manual",
        }
    }
}

/// Detected restart mode plus the command to show the operator.
#[derive(Clone)]
pub struct RestartInfo {
    pub mode: RestartMode,
    pub hint: String,
}

fn env_present(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

fn is_container() -> bool {
    // Any positive signal wins: a false "not a container" is the dangerous
    // case (exiting PID 1 with no restart policy tears the container down).
    std::path::Path::new("/.dockerenv").exists()
        || std::process::id() == 1
        || std::fs::read_to_string("/proc/1/cgroup").is_ok_and(|s| {
            s.contains("docker") || s.contains("containerd") || s.contains("kubepods")
        })
}

/// Classify the runtime environment to pick exit-vs-manual and hint text.
///
/// This only chooses what to *show*; the gateway does not act on it in Phase 1.
/// The classification is static for the process lifetime (env vars + cgroup), so
/// it is computed once and cached — `/api/status` calls this on every poll.
pub fn detect_restart() -> RestartInfo {
    static CACHE: OnceLock<RestartInfo> = OnceLock::new();
    CACHE.get_or_init(detect_restart_uncached).clone()
}

fn detect_restart_uncached() -> RestartInfo {
    // Container first — default to manual since we can't see a restart policy.
    if is_container() {
        let hint = if env_present("KUBERNETES_SERVICE_HOST") {
            "kubectl rollout restart deployment/zeroclaw"
        } else {
            "docker compose restart"
        };
        return RestartInfo {
            mode: RestartMode::Manual,
            hint: hint.to_string(),
        };
    }
    // systemd: a clean exit is relaunched when the unit sets Restart=on-success.
    if env_present("INVOCATION_ID") || env_present("JOURNAL_STREAM") {
        return RestartInfo {
            mode: RestartMode::Supervised,
            hint: "systemctl restart zeroclaw".to_string(),
        };
    }
    // launchd (macOS): KeepAlive relaunches on exit.
    if cfg!(target_os = "macos") && env_present("XPC_SERVICE_NAME") {
        return RestartInfo {
            mode: RestartMode::Supervised,
            hint: "launchctl kickstart -k <your-zeroclaw-label>".to_string(),
        };
    }
    RestartInfo {
        mode: RestartMode::Manual,
        hint: "restart the `zeroclaw daemon` process".to_string(),
    }
}

// ── Version check ────────────────────────────────────────────────

/// Parsed output of `zeroclaw update --check --json`. Field names must match
/// the JSON emitted in `src/main.rs`.
#[derive(Debug, Clone, Deserialize)]
struct CliCheck {
    current_version: String,
    latest_version: String,
    is_newer: bool,
    release_url: Option<String>,
    release_notes: Option<String>,
    published_at: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct CheckQuery {
    /// Bypass the 1h cache and re-query GitHub.
    #[serde(default)]
    pub force: bool,
    /// Check a specific release tag instead of latest.
    #[serde(default)]
    pub version: Option<String>,
}

/// Cache for the latest-version check. Specific-version queries are not cached.
static CHECK_CACHE: Mutex<Option<(Instant, CliCheck)>> = Mutex::new(None);

fn check_to_json(info: &CliCheck) -> serde_json::Value {
    serde_json::json!({
        "current_version": info.current_version,
        "latest_version": info.latest_version,
        "is_newer": info.is_newer,
        "release_url": info.release_url,
        "release_notes": info.release_notes,
        "published_at": info.published_at,
    })
}

async fn run_cli_check(version: Option<&str>) -> anyhow::Result<CliCheck> {
    let exe = std::env::current_exe().context("cannot determine current executable path")?;
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("update").arg("--check").arg("--json");
    if let Some(v) = version {
        cmd.arg("--version").arg(v);
    }
    cmd.stdin(std::process::Stdio::null());

    let output = tokio::time::timeout(CHECK_TIMEOUT, cmd.output())
        .await
        .context("version check timed out")?
        .context("failed to spawn `zeroclaw update --check`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("update --check failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<CliCheck>(stdout.trim())
        .context("failed to parse `update --check --json` output")
}

/// GET /api/version/check[?force=true][&version=X]
///
/// Never fails the dashboard: on any error it returns 200 with
/// `{ is_newer: false, error }` so the version tag degrades gracefully.
pub async fn handle_version_check(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CheckQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let use_cache = !q.force && q.version.is_none();
    if use_cache {
        if let Some((ts, cached)) = CHECK_CACHE.lock().unwrap().as_ref() {
            if ts.elapsed() < CHECK_CACHE_TTL {
                return Json(check_to_json(cached)).into_response();
            }
        }
    }

    match run_cli_check(q.version.as_deref()).await {
        Ok(info) => {
            if use_cache {
                *CHECK_CACHE.lock().unwrap() = Some((Instant::now(), info.clone()));
            }
            Json(check_to_json(&info)).into_response()
        }
        Err(e) => Json(serde_json::json!({
            "current_version": env!("CARGO_PKG_VERSION"),
            "latest_version": serde_json::Value::Null,
            "is_newer": false,
            "error": e.to_string(),
        }))
        .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_mode_as_str_is_stable() {
        assert_eq!(RestartMode::Supervised.as_str(), "supervised");
        assert_eq!(RestartMode::Manual.as_str(), "manual");
    }

    #[test]
    fn detect_restart_returns_a_nonempty_hint() {
        let info = detect_restart();
        assert!(!info.hint.is_empty());
    }

    #[test]
    fn cli_check_json_roundtrips() {
        let json = r#"{
            "current_version": "0.7.3",
            "latest_version": "0.7.4",
            "is_newer": true,
            "release_url": "https://example.com/r",
            "release_notes": "- fix things",
            "published_at": "2026-06-20T00:00:00Z"
        }"#;
        let parsed: CliCheck = serde_json::from_str(json).unwrap();
        assert!(parsed.is_newer);
        assert_eq!(parsed.latest_version, "0.7.4");
        let out = check_to_json(&parsed);
        assert_eq!(out["latest_version"], "0.7.4");
        assert_eq!(out["is_newer"], true);
    }
}
