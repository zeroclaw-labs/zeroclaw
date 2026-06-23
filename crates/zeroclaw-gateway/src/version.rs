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
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};
use serde::Deserialize;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

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

// ── Upgrade (Phase 2/3) ──────────────────────────────────────────

/// Lifecycle of an in-flight upgrade. `Restarting` is only reached when the
/// caller opted into auto-restart under a supervisor (Phase 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpgradeState {
    Running,
    Done,
    Restarting,
    Failed,
}

impl UpgradeState {
    fn as_str(self) -> &'static str {
        match self {
            UpgradeState::Running => "running",
            UpgradeState::Done => "done",
            UpgradeState::Restarting => "restarting",
            UpgradeState::Failed => "failed",
        }
    }

    fn is_terminal(self) -> bool {
        matches!(self, UpgradeState::Done | UpgradeState::Failed)
    }
}

/// Shared progress for the current/most-recent upgrade. The spawned task writes
/// it; `GET /api/version/upgrade/status` reads it.
struct UpgradeProgress {
    handoff_id: String,
    state: UpgradeState,
    /// 0 before the first `Phase N/6` marker, else 1..=6.
    phase: u8,
    /// Last ~50 lines of combined stdout/stderr.
    log_tail: VecDeque<String>,
    previous_version: String,
    target_version: Option<String>,
    error: Option<String>,
    restart_mode: &'static str,
    restart_hint: String,
}

const LOG_TAIL_MAX: usize = 50;
/// Grace before the supervised self-restart, so the final status poll flushes.
const RESTART_GRACE: Duration = Duration::from_millis(1500);
/// Upper bound on the whole upgrade subprocess (download + verify + swap).
const UPGRADE_TIMEOUT: Duration = Duration::from_secs(900);

/// The single in-flight (or most recent) upgrade. Only one runs at a time.
static UPGRADE: Mutex<Option<Arc<Mutex<UpgradeProgress>>>> = Mutex::new(None);

#[derive(Debug, Default, Deserialize)]
pub struct UpgradeRequest {
    /// Target release tag; defaults to latest.
    #[serde(default)]
    pub version: Option<String>,
    /// After a successful swap, exit so a supervisor relaunches the new binary.
    /// Only honoured under a detected supervisor (systemd/launchd).
    #[serde(default)]
    pub auto_restart: bool,
}

fn json_error(code: StatusCode, msg: &str) -> axum::response::Response {
    (code, Json(serde_json::json!({ "error": msg }))).into_response()
}

/// POST /api/version/upgrade — apply an upgrade via `zeroclaw update`.
///
/// Returns 202 with a `handoff_id`; the work runs on a detached task and the
/// client polls `GET /api/version/upgrade/status`.
pub async fn handle_version_upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    // Accept an empty body (defaults) or a JSON object.
    let req: UpgradeRequest = if body.is_empty() {
        UpgradeRequest::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => return json_error(StatusCode::BAD_REQUEST, &format!("invalid body: {e}")),
        }
    };

    if !state.config.read().gateway.allow_self_upgrade {
        return json_error(
            StatusCode::FORBIDDEN,
            "self-upgrade is disabled; set gateway.allow_self_upgrade = true to enable it",
        );
    }

    let restart = detect_restart();
    let supervised = restart.mode == RestartMode::Supervised;
    if req.auto_restart && !supervised {
        return json_error(
            StatusCode::BAD_REQUEST,
            "auto_restart requires a supervised environment (systemd/launchd); restart manually instead",
        );
    }

    // One upgrade at a time.
    let mut slot = UPGRADE.lock().unwrap();
    if let Some(existing) = slot.as_ref() {
        if !existing.lock().unwrap().state.is_terminal() {
            return json_error(StatusCode::CONFLICT, "an upgrade is already in progress");
        }
    }

    let handoff_id = uuid::Uuid::new_v4().to_string();
    let progress = Arc::new(Mutex::new(UpgradeProgress {
        handoff_id: handoff_id.clone(),
        state: UpgradeState::Running,
        phase: 0,
        log_tail: VecDeque::new(),
        previous_version: env!("CARGO_PKG_VERSION").to_string(),
        target_version: req.version.clone(),
        error: None,
        restart_mode: restart.mode.as_str(),
        restart_hint: restart.hint,
    }));
    *slot = Some(progress.clone());
    drop(slot);

    ::zeroclaw_spawn::spawn!(run_upgrade(
        progress,
        req.version,
        req.auto_restart && supervised,
    ));

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "handoff_id": handoff_id })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct UpgradeStatusQuery {
    #[serde(default)]
    pub handoff_id: Option<String>,
}

/// GET /api/version/upgrade/status[?handoff_id=X]
pub async fn handle_version_upgrade_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<UpgradeStatusQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let slot = UPGRADE.lock().unwrap();
    let Some(progress) = slot.as_ref() else {
        return Json(serde_json::json!({ "state": "idle" })).into_response();
    };
    let p = progress.lock().unwrap();
    if let Some(id) = q.handoff_id.as_deref() {
        if id != p.handoff_id {
            return json_error(StatusCode::NOT_FOUND, "unknown handoff_id");
        }
    }
    Json(serde_json::json!({
        "handoff_id": p.handoff_id,
        "state": p.state.as_str(),
        "phase": p.phase,
        "log_tail": p.log_tail,
        "previous_version": p.previous_version,
        "target_version": p.target_version,
        "restart_mode": p.restart_mode,
        "restart_hint": p.restart_hint,
        "error": p.error,
    }))
    .into_response()
}

fn set_state(progress: &Arc<Mutex<UpgradeProgress>>, state: UpgradeState) {
    progress.lock().unwrap().state = state;
}

fn fail(progress: &Arc<Mutex<UpgradeProgress>>, msg: String) {
    let mut p = progress.lock().unwrap();
    p.state = UpgradeState::Failed;
    p.error = Some(msg);
}

/// Parse a `Phase N/6` marker out of a log line (best-effort).
fn parse_phase(line: &str) -> Option<u8> {
    let rest = line.split_once("Phase ")?.1;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|n| (1..=6).contains(n))
}

/// Strip ANSI escape sequences so the dashboard log panel renders clean text.
/// `--verbose` makes the child's tracing layer emit colour codes (CSI `ESC[…m`),
/// which show up as garbage in a browser.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // ESC: drop a CSI sequence (`[` … final byte 0x40..=0x7e); for any
        // other escape, just drop the following byte (best-effort).
        if chars.peek() == Some(&'[') {
            chars.next();
            for nc in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&nc) {
                    break;
                }
            }
        } else {
            chars.next();
        }
    }
    out
}

/// Stream a child's output into the log ring buffer, advancing `phase`.
async fn pump_lines<R: AsyncRead + Unpin>(reader: R, progress: Arc<Mutex<UpgradeProgress>>) {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(raw)) = lines.next_line().await {
        let line = strip_ansi(&raw);
        let mut p = progress.lock().unwrap();
        if let Some(phase) = parse_phase(&line) {
            p.phase = phase;
        }
        p.log_tail.push_back(line);
        while p.log_tail.len() > LOG_TAIL_MAX {
            p.log_tail.pop_front();
        }
    }
}

/// Drive `zeroclaw update`, then either mark done or (Phase 3) self-restart.
async fn run_upgrade(
    progress: Arc<Mutex<UpgradeProgress>>,
    version: Option<String>,
    auto_restart_supervised: bool,
) {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            fail(
                &progress,
                format!("cannot determine current executable: {e}"),
            );
            return;
        }
    };

    let mut cmd = tokio::process::Command::new(exe);
    // `--verbose` surfaces the `Phase N/6` records (otherwise INFO logs are
    // muted on the child's stderr), so the dashboard can stream progress.
    cmd.arg("--verbose").arg("update");
    if let Some(v) = &version {
        cmd.arg("--version").arg(v);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            fail(&progress, format!("failed to start `zeroclaw update`: {e}"));
            return;
        }
    };

    // Clone into locals first: `spawn!` wraps the body in `async move`, so a
    // `progress.clone()` *inside* the macro would move `progress` itself.
    let mut pumps = Vec::new();
    if let Some(out) = child.stdout.take() {
        let p = progress.clone();
        pumps.push(::zeroclaw_spawn::spawn!(pump_lines(out, p)));
    }
    if let Some(err) = child.stderr.take() {
        let p = progress.clone();
        pumps.push(::zeroclaw_spawn::spawn!(pump_lines(err, p)));
    }

    let status = match tokio::time::timeout(UPGRADE_TIMEOUT, child.wait()).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            fail(&progress, format!("update process error: {e}"));
            return;
        }
        Err(_) => {
            let _ = child.start_kill();
            fail(
                &progress,
                "update timed out after 15 minutes; the old binary is unchanged".to_string(),
            );
            return;
        }
    };
    for h in pumps {
        let _ = h.await;
    }

    if !status.success() {
        let tail = {
            let p = progress.lock().unwrap();
            p.log_tail
                .iter()
                .rev()
                .take(5)
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ")
        };
        fail(
            &progress,
            format!("update failed ({status}); the previous binary is preserved. {tail}"),
        );
        return;
    }

    if auto_restart_supervised {
        // Phase 3: the binary on disk is new; exit cleanly so the supervisor
        // relaunches it. We never spawn/exec a replacement ourselves.
        set_state(&progress, UpgradeState::Restarting);
        tokio::time::sleep(RESTART_GRACE).await;
        request_supervised_restart();
    } else {
        set_state(&progress, UpgradeState::Done);
    }
}

/// Send ourselves SIGTERM so the daemon's signal handler runs its graceful
/// teardown (`DaemonExit::Shutdown`) and the supervisor relaunches the new
/// binary. No-op on non-unix (auto_restart is only offered when supervised,
/// which we only detect on unix).
fn request_supervised_restart() {
    #[cfg(unix)]
    // SAFETY: `raise` is async-signal-safe and merely posts SIGTERM to this
    // process, which the daemon already handles for graceful shutdown.
    unsafe {
        libc::raise(libc::SIGTERM);
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

    #[test]
    fn upgrade_state_strings_and_terminality() {
        assert_eq!(UpgradeState::Running.as_str(), "running");
        assert_eq!(UpgradeState::Restarting.as_str(), "restarting");
        assert!(UpgradeState::Done.is_terminal());
        assert!(UpgradeState::Failed.is_terminal());
        assert!(!UpgradeState::Running.is_terminal());
        assert!(!UpgradeState::Restarting.is_terminal());
    }

    #[test]
    fn parse_phase_extracts_marker() {
        assert_eq!(parse_phase("Phase 1/6: Preflight checks..."), Some(1));
        assert_eq!(parse_phase("  INFO  Phase 6/6: Cleanup"), Some(6));
        assert_eq!(parse_phase("no marker here"), None);
        assert_eq!(parse_phase("Phase 9/6: bogus"), None);
    }

    #[test]
    fn strip_ansi_removes_color_codes_and_keeps_text() {
        let raw = "\u{1b}[2m2026-06-23T07:14:41Z\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m \
                   Phase 2/6: Downloading...";
        let clean = strip_ansi(raw);
        assert!(!clean.contains('\u{1b}'), "ESC remained: {clean:?}");
        assert!(!clean.contains("[2m") && !clean.contains("[32m"));
        assert!(clean.contains("Phase 2/6: Downloading..."));
        assert_eq!(parse_phase(&clean), Some(2));
    }
}
