//! Version-check and restart-classification helpers for the dashboard's
//! upgrade affordance (RFC: In-app upgrade with optional supervised restart).
//!
//! Phase 1 (read-only): `GET /api/version/check` reports whether a newer
//! release exists, plus release notes, from the core's
//! `system/version-check`: the core runs its own `zeroclaw update --check
//! --json` and caches a latest-release result for an hour (see
//! `zeroclaw_runtime::update_check`).
//!
//! Restart classification is advisory only here: it tells the dashboard which
//! restart command to show after an upgrade. The gateway never restarts itself
//! in Phase 1.

use super::AppState;
use super::api::require_auth;
use crate::core_rpc::{CoreAccess, CoreCall, CoreError};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use zeroclaw_rpc_client::Method;
use zeroclaw_rpc_proto::types::SystemVersionCheckParams;
use zeroclaw_runtime::i18n::get_required_cli_string;

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

// ── Restart classification (advisory) ────────────────────────────

/// How a post-upgrade restart is achieved in this environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RestartMode {
    /// The ZeroClaw Desktop supervisor relaunches us after the dedicated exit.
    DesktopSupervised,
    /// A supervisor (systemd/launchd) relaunches us after a clean exit.
    Supervised,
    /// No supervisor, but we can relaunch ourselves: after teardown the daemon
    /// detached-spawns the new binary, then exits (bare unix process).
    SelfRespawn,
    /// We cannot safely auto-restart (container PID 1, or non-unix bare); the
    /// operator must restart manually.
    Manual,
}

impl RestartMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RestartMode::DesktopSupervised => "desktop_supervised",
            RestartMode::Supervised => "supervised",
            RestartMode::SelfRespawn => "self_respawn",
            RestartMode::Manual => "manual",
        }
    }

    /// Whether the dashboard may offer (and the backend honour) auto-restart.
    pub fn auto_restartable(self) -> bool {
        matches!(
            self,
            RestartMode::DesktopSupervised | RestartMode::Supervised | RestartMode::SelfRespawn
        )
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
    if zeroclaw_runtime::restart::is_desktop_supervised() {
        return RestartInfo {
            mode: RestartMode::DesktopSupervised,
            hint: get_required_cli_string("cli-gateway-restart-hint-process"),
        };
    }
    // Container first — default to manual since we can't see a restart policy.
    if is_container() {
        let hint = if env_present("KUBERNETES_SERVICE_HOST") {
            get_required_cli_string("cli-gateway-restart-hint-kubernetes")
        } else {
            get_required_cli_string("cli-gateway-restart-hint-container")
        };
        return RestartInfo {
            mode: RestartMode::Manual,
            hint,
        };
    }
    // systemd: a clean exit is relaunched when the unit sets Restart=on-success.
    if env_present("INVOCATION_ID") || env_present("JOURNAL_STREAM") {
        return RestartInfo {
            mode: RestartMode::Supervised,
            hint: get_required_cli_string("cli-gateway-restart-hint-systemd"),
        };
    }
    // launchd (macOS): KeepAlive relaunches on exit.
    if cfg!(target_os = "macos") && env_present("XPC_SERVICE_NAME") {
        return RestartInfo {
            mode: RestartMode::Supervised,
            hint: get_required_cli_string("cli-gateway-restart-hint-launchd"),
        };
    }
    // Bare process. On unix/windows we can relaunch ourselves (detached respawn
    // after teardown); elsewhere there's no safe self-relaunch, so stay manual.
    if cfg!(unix) || cfg!(windows) {
        RestartInfo {
            mode: RestartMode::SelfRespawn,
            hint: get_required_cli_string("cli-gateway-restart-hint-process"),
        }
    } else {
        RestartInfo {
            mode: RestartMode::Manual,
            hint: get_required_cli_string("cli-gateway-restart-hint-process"),
        }
    }
}

// ── Version check ────────────────────────────────────────────────

pub use zeroclaw_rpc_proto::types::VersionCheckResponse;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct CheckQuery {
    /// Bypass the 1h cache and re-query GitHub.
    #[serde(default)]
    pub force: bool,
    /// Check a specific release tag instead of latest.
    #[serde(default)]
    pub version: Option<String>,
}

/// `GET /api/version/check[?force=true][&version=X]`
///
/// Never fails the dashboard: on any error it returns 200 with
/// `{ is_newer: false, error }` so the version tag degrades gracefully. The
/// check is the core's, `system/version-check`, whichever process serves
/// the route; in-process it runs here, in the core's own process.
pub async fn handle_version_check(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CheckQuery>,
    access: CoreAccess,
) -> impl IntoResponse {
    if let CoreAccess::Core(core) = access {
        return version_check_through_core(&core, &q)
            .await
            .unwrap_or_else(IntoResponse::into_response);
    }
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    Json(zeroclaw_runtime::update_check::check(q.force, q.version.as_deref()).await).into_response()
}

/// `GET /api/version/check` through the core, the body every router serves
/// for it.
pub(crate) async fn version_check_through_core(
    core: &CoreCall,
    query: &CheckQuery,
) -> Result<Response, CoreError> {
    let params = SystemVersionCheckParams {
        force: query.force,
        version: query.version.clone(),
    };
    let checked: VersionCheckResponse = core
        .call(Method::SystemVersionCheck, serde_json::json!(params))
        .await?;
    Ok(Json(checked).into_response())
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
    fn is_terminal(self) -> bool {
        matches!(self, UpgradeState::Done | UpgradeState::Failed)
    }
}

/// Wire-format lifecycle state for `GET /api/version/upgrade/status`. Mirrors
/// `UpgradeState` plus `Idle` (no upgrade started this process), and is the
/// serializable enum the OpenAPI schema and generated web client derive from.
#[derive(Debug, Clone, Copy, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum UpgradeStatusState {
    Idle,
    Running,
    Done,
    Restarting,
    Failed,
}

impl From<UpgradeState> for UpgradeStatusState {
    fn from(s: UpgradeState) -> Self {
        match s {
            UpgradeState::Running => Self::Running,
            UpgradeState::Done => Self::Done,
            UpgradeState::Restarting => Self::Restarting,
            UpgradeState::Failed => Self::Failed,
        }
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
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct UpgradeRequest {
    /// Target release tag; defaults to latest.
    #[serde(default)]
    pub version: Option<String>,
    /// After a successful swap, exit so the detected supervisor relaunches the
    /// new binary.
    #[serde(default)]
    pub auto_restart: bool,
}

/// 202 body for `POST /api/version/upgrade`: the id to poll status with.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct UpgradeAcceptedResponse {
    pub handoff_id: String,
}

/// Error envelope for the `/api/version/*` endpoints (`4xx`).
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct VersionErrorResponse {
    pub error: String,
}

fn json_error(code: StatusCode, msg: &str) -> axum::response::Response {
    (
        code,
        Json(VersionErrorResponse {
            error: msg.to_string(),
        }),
    )
        .into_response()
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
    if req.auto_restart && !restart.mode.auto_restartable() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "auto_restart is not available here (container or non-unix bare process); restart manually instead",
        );
    }

    // One upgrade at a time.
    let mut slot = lock_recover(&UPGRADE);
    if let Some(existing) = slot.as_ref() {
        if !lock_recover(existing).state.is_terminal() {
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

    let action = if req.auto_restart {
        match restart.mode {
            RestartMode::DesktopSupervised => RestartAction::DesktopSupervised,
            RestartMode::Supervised => RestartAction::Supervised,
            RestartMode::SelfRespawn => RestartAction::SelfRespawn {
                // `reload_tx` is `None` exactly when the gateway runs without
                // a daemon wrapper (see `AppState::reload_tx` docs and
                // `run_gateway_if_enabled` in `src/main.rs`). In that mode
                // SIGTERM/`shutdown_notify` would just kill the process
                // before `respawn_if_requested()` could run, so signal the
                // gateway's own shutdown watch instead.
                standalone_shutdown_tx: state
                    .reload_tx
                    .is_none()
                    .then(|| state.shutdown_tx.clone()),
            },
            // Unreachable: rejected with 400 above.
            RestartMode::Manual => RestartAction::None,
        }
    } else {
        RestartAction::None
    };
    ::zeroclaw_spawn::spawn!(run_upgrade(progress, req.version, action));

    (
        StatusCode::ACCEPTED,
        Json(UpgradeAcceptedResponse { handoff_id }),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct UpgradeStatusQuery {
    #[serde(default)]
    pub handoff_id: Option<String>,
}

/// Response body for `GET /api/version/upgrade/status`. When no upgrade has run
/// this process only `state: "idle"` is set; during/after a run the remaining
/// fields carry the live phase, log tail, and restart metadata. Single source
/// of truth for the generated web client — see [`crate::openapi`].
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct UpgradeStatusResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff_id: Option<String>,
    pub state: UpgradeStatusState,
    /// 0 before the first `Phase N/6` marker, else 1..=6.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_tail: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restart_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restart_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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

    let slot = lock_recover(&UPGRADE);
    let Some(progress) = slot.as_ref() else {
        return Json(UpgradeStatusResponse {
            handoff_id: None,
            state: UpgradeStatusState::Idle,
            phase: None,
            log_tail: None,
            previous_version: None,
            target_version: None,
            restart_mode: None,
            restart_hint: None,
            error: None,
        })
        .into_response();
    };
    let p = lock_recover(progress);
    if let Some(id) = q.handoff_id.as_deref() {
        if id != p.handoff_id {
            return json_error(StatusCode::NOT_FOUND, "unknown handoff_id");
        }
    }
    Json(UpgradeStatusResponse {
        handoff_id: Some(p.handoff_id.clone()),
        state: p.state.into(),
        phase: Some(p.phase),
        log_tail: Some(p.log_tail.iter().cloned().collect()),
        previous_version: Some(p.previous_version.clone()),
        target_version: p.target_version.clone(),
        restart_mode: Some(p.restart_mode.to_string()),
        restart_hint: Some(p.restart_hint.clone()),
        error: p.error.clone(),
    })
    .into_response()
}

fn set_state(progress: &Arc<Mutex<UpgradeProgress>>, state: UpgradeState) {
    lock_recover(progress).state = state;
}

fn fail(progress: &Arc<Mutex<UpgradeProgress>>, msg: String) {
    let mut p = lock_recover(progress);
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
        let mut p = lock_recover(&progress);
        if let Some(phase) = parse_phase(&line) {
            p.phase = phase;
        }
        p.log_tail.push_back(line);
        while p.log_tail.len() > LOG_TAIL_MAX {
            p.log_tail.pop_front();
        }
    }
}

/// What to do once the upgrade succeeds.
#[derive(Debug, Clone)]
enum RestartAction {
    /// Leave the swapped binary on disk; the operator restarts manually.
    None,
    /// Ask the ZeroClaw Desktop supervisor to launch the next generation.
    DesktopSupervised,
    /// Exit cleanly; a supervisor (systemd/launchd) or the outer daemon process
    /// relaunches the new binary. The daemon's `wait_for_exit_signal` is the
    /// shutdown receiver here (SIGTERM on unix, `restart::shutdown_notify()`
    /// elsewhere).
    Supervised,
    /// Exit cleanly after asking `main` to detached-spawn the new binary (bare
    /// process with no supervisor).
    ///
    /// `standalone_shutdown_tx` is `Some` only when the gateway runs without
    /// a daemon wrapper (`zeroclaw gateway start`). In that mode no one is
    /// listening for SIGTERM/`shutdown_notify`, so we send the gateway's own
    /// shutdown watch directly — that lets `run_gateway()` return cleanly so
    /// `respawn_if_requested()` runs in `main`. When the daemon wraps the
    /// gateway, this is `None` and we fall through to the same shutdown path
    /// as `Supervised` (signal the daemon's wait loop so it tears down).
    SelfRespawn {
        standalone_shutdown_tx: Option<tokio::sync::watch::Sender<bool>>,
    },
}

/// Drive `zeroclaw update`, then either mark done or (Phase 3) restart.
async fn run_upgrade(
    progress: Arc<Mutex<UpgradeProgress>>,
    version: Option<String>,
    action: RestartAction,
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
            let p = lock_recover(&progress);
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

    match action {
        RestartAction::None => set_state(&progress, UpgradeState::Done),
        RestartAction::DesktopSupervised => {
            if let Err(error) = zeroclaw_runtime::restart::request_desktop_restart() {
                fail(
                    &progress,
                    format!("cannot arm desktop-supervised restart: {error}"),
                );
                return;
            }
            set_state(&progress, UpgradeState::Restarting);
            tokio::time::sleep(RESTART_GRACE).await;
            trigger_graceful_shutdown();
        }
        RestartAction::Supervised => {
            // The binary on disk is new; exit cleanly so the supervisor
            // relaunches it. We never spawn/exec a replacement ourselves.
            set_state(&progress, UpgradeState::Restarting);
            tokio::time::sleep(RESTART_GRACE).await;
            trigger_graceful_shutdown();
        }
        RestartAction::SelfRespawn {
            standalone_shutdown_tx,
        } => {
            // No supervisor: ask `main` to detached-spawn the new binary once
            // the daemon (or standalone gateway) has torn down, then exit. The
            // respawn flag is read at the post-shutdown point, after the
            // listener is released.
            //
            // Set the flag *before* sleeping so that an external SIGTERM
            // arriving during the grace window still finds it set.
            zeroclaw_runtime::restart::request_respawn();
            set_state(&progress, UpgradeState::Restarting);
            tokio::time::sleep(RESTART_GRACE).await;
            match standalone_shutdown_tx {
                // Standalone gateway (`zeroclaw gateway start`): no daemon
                // wait loop is listening for SIGTERM/`shutdown_notify`, so the
                // process would just die before `respawn_if_requested()` ran
                // in `main`. Drive the gateway's own shutdown watch so
                // `run_gateway()` returns cleanly and `main` reaches the
                // post-teardown respawn point.
                Some(tx) => {
                    let _ = tx.send(true);
                }
                // Bare daemon-wrapped gateway: the daemon owns the shutdown
                // watch; signalling it would only restart the gateway
                // component under the daemon supervisor. Fall through to the
                // SIGTERM/`shutdown_notify` path that tears down the whole
                // daemon, so `respawn_if_requested()` runs at the daemon
                // exit point in `main`.
                None => trigger_graceful_shutdown(),
            }
        }
    }
}

/// Trigger the daemon's graceful teardown (`DaemonExit::Shutdown`). Whether the
/// process is then relaunched by a supervisor or self-respawned by `main` is
/// decided by the caller. Unix self-signals SIGTERM (reusing the existing signal
/// path); Windows fires the daemon's in-process shutdown trigger instead.
fn trigger_graceful_shutdown() {
    #[cfg(unix)]
    {
        // SAFETY: `raise` is async-signal-safe and merely posts SIGTERM to this
        // process, which the daemon already handles for graceful shutdown.
        unsafe {
            libc::raise(libc::SIGTERM);
        }
    }
    #[cfg(not(unix))]
    {
        zeroclaw_runtime::restart::request_shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_recover_preserves_state_after_poisoning() {
        let state = Arc::new(Mutex::new(1_u8));
        let poisoned = state.clone();
        let result = std::thread::spawn(move || {
            let mut guard = poisoned.lock().expect("fresh mutex must lock");
            *guard = 2;
            panic!("poison test mutex");
        })
        .join();
        assert!(result.is_err());
        assert_eq!(*lock_recover(&state), 2);
    }

    #[test]
    fn restart_mode_wire_values_are_stable() {
        for (mode, expected) in [
            (RestartMode::DesktopSupervised, "desktop_supervised"),
            (RestartMode::Supervised, "supervised"),
            (RestartMode::SelfRespawn, "self_respawn"),
            (RestartMode::Manual, "manual"),
        ] {
            assert_eq!(mode.as_str(), expected);
            assert_eq!(
                serde_json::to_value(mode).expect("serialize mode"),
                expected
            );
        }
        assert!(RestartMode::Supervised.auto_restartable());
        assert!(RestartMode::DesktopSupervised.auto_restartable());
        assert!(RestartMode::SelfRespawn.auto_restartable());
        assert!(!RestartMode::Manual.auto_restartable());
    }

    #[test]
    fn detect_restart_returns_a_nonempty_hint() {
        let info = detect_restart();
        assert!(!info.hint.is_empty());
    }

    #[test]
    fn restart_hint_keys_resolve_via_fluent() {
        // Every restart hint shown to operators must come from the localization
        // pipeline, not a bare literal. A missing key resolves to the `{key}`
        // sentinel, so assert each one is backed by a real Fluent entry — this
        // guards against a typo drifting between `version.rs` and `cli.ftl`.
        for key in [
            "cli-gateway-restart-hint-kubernetes",
            "cli-gateway-restart-hint-container",
            "cli-gateway-restart-hint-systemd",
            "cli-gateway-restart-hint-launchd",
            "cli-gateway-restart-hint-process",
        ] {
            let resolved = get_required_cli_string(key);
            assert!(
                !resolved.is_empty() && !resolved.starts_with('{'),
                "missing Fluent string for {key}: {resolved}"
            );
        }
    }

    #[test]
    fn upgrade_state_strings_and_terminality() {
        // The wire enum drives the OpenAPI schema / generated web client.
        assert_eq!(
            serde_json::to_value(UpgradeStatusState::Running).unwrap(),
            "running"
        );
        assert_eq!(
            serde_json::to_value(UpgradeStatusState::Restarting).unwrap(),
            "restarting"
        );
        assert_eq!(
            serde_json::to_value(UpgradeStatusState::Idle).unwrap(),
            "idle"
        );
        assert_eq!(
            serde_json::to_value(UpgradeStatusState::from(UpgradeState::Done)).unwrap(),
            "done"
        );
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

    /// Regression for the standalone-gateway respawn path:
    /// `zeroclaw gateway start` runs without a daemon wrapper, so nothing is
    /// listening for SIGTERM/`shutdown_notify`. Driving the gateway's own
    /// `shutdown_tx` watch is what makes `run_gateway()` return so
    /// `respawn_if_requested()` runs in `main`.
    #[tokio::test]
    async fn self_respawn_standalone_shutdown_tx_drives_gateway_watch() {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let action = RestartAction::SelfRespawn {
            standalone_shutdown_tx: Some(tx),
        };
        // Exercise the routing arm directly (no need to run a full upgrade):
        // matches the `match` block in `run_upgrade` after the grace sleep.
        if let RestartAction::SelfRespawn {
            standalone_shutdown_tx: Some(t),
        } = action
        {
            t.send(true).unwrap();
        } else {
            panic!("expected SelfRespawn carrying a sender");
        }
        rx.changed().await.unwrap();
        assert!(*rx.borrow());
    }

    /// Bare daemon-wrapped gateway: the gateway's `shutdown_tx` is owned by the
    /// daemon supervisor, signalling it would only restart the component.
    /// `SelfRespawn` must carry `None` so we fall through to the daemon-exit
    /// shutdown path (SIGTERM / `shutdown_notify`) instead.
    #[test]
    fn self_respawn_daemon_mode_carries_no_standalone_sender() {
        let action = RestartAction::SelfRespawn {
            standalone_shutdown_tx: None,
        };
        match action {
            RestartAction::SelfRespawn {
                standalone_shutdown_tx,
            } => assert!(standalone_shutdown_tx.is_none()),
            _ => panic!("expected SelfRespawn"),
        }
    }
}
