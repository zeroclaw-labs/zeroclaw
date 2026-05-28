//! Background snapshot tracker — per-agent shadow-git captures on a fixed
//! interval, plus periodic cleanup of unreachable objects.
//!
//! Driven by `config.snapshot.auto_track_enabled`. When off, both the capture
//! loop and the cleanup loop are no-ops and the daemon does not spawn the
//! task. When on, the tracker walks every configured agent on each tick,
//! captures `agent_workspace_dir(&alias)`, and logs the resulting tree hash
//! through `::zeroclaw_log::record!` so operators can locate it from
//! `zeroclaw snapshot patch <hash>` or `… undo`.
//!
//! Lives outside the `run_tool_call_loop` signature on purpose — the loop
//! already takes ~29 positional parameters and is the subject of a separate
//! refactor (`Plans/glimmering-mixing-moore-v2.md` Slice B). Treating
//! auto-tracking as a separate daemon-spawned task lets it ship today
//! without joining that queue.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use zeroclaw_config::schema::Config;

use super::registry::get_or_create;

/// Floor below which `auto_track_interval_secs` would thrash the worktree
/// without producing useful diffs. Misconfigurations clamp up to this.
const MIN_INTERVAL_SECS: u64 = 15;

/// Spawn the background snapshot tracker. Returns `None` when auto-tracking
/// is disabled in config — caller should skip the join-handle bookkeeping.
///
/// Two intervals run inside the same task:
/// - `auto_track_interval_secs` — per-agent capture
/// - `cleanup_interval_hours * 3600` — `git gc --prune=7.days` on each repo
pub fn spawn_tracker(config: Config) -> Option<JoinHandle<()>> {
    if !config.snapshot.auto_track_enabled {
        return None;
    }

    let interval_secs = config
        .snapshot
        .auto_track_interval_secs
        .max(MIN_INTERVAL_SECS);
    let cleanup_interval_hours = config.snapshot.cleanup_interval_hours;
    let cleanup_enabled = cleanup_interval_hours > 0;

    let data_dir = config.data_dir.clone();
    let agent_aliases: Vec<String> = config.agents.keys().cloned().collect();
    // Resolve workspace dirs up-front so the loop body doesn't need to
    // re-borrow the cloned `Config` on every tick.
    let workspaces: Vec<(String, std::path::PathBuf)> = agent_aliases
        .iter()
        .map(|alias| (alias.clone(), config.agent_workspace_dir(alias)))
        .collect();

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "agents": workspaces.len(),
                "interval_secs": interval_secs,
                "cleanup_interval_hours": cleanup_interval_hours,
                "data_dir": data_dir.display().to_string(),
            })
        ),
        "snapshot auto-tracker starting"
    );

    Some(tokio::spawn(async move {
        let mut capture_ticker = tokio::time::interval(Duration::from_secs(interval_secs));
        let mut cleanup_ticker = tokio::time::interval(Duration::from_secs(
            cleanup_interval_hours.saturating_mul(3600).max(1),
        ));
        // Skip the immediate first tick on both — let the daemon settle.
        capture_ticker.tick().await;
        cleanup_ticker.tick().await;

        loop {
            tokio::select! {
                _ = capture_ticker.tick() => {
                    run_capture_pass(&workspaces, &data_dir).await;
                }
                _ = cleanup_ticker.tick(), if cleanup_enabled => {
                    run_cleanup_pass(&workspaces, &data_dir).await;
                }
            }
        }
    }))
}

async fn run_capture_pass(workspaces: &[(String, std::path::PathBuf)], data_dir: &Path) {
    for (alias, workspace) in workspaces {
        let Some(snap) = get_or_create(workspace, data_dir) else {
            // `for_session` already logs the gate-fail reason (no git, no PATH).
            // Skip silently here so we don't double-log every tick.
            continue;
        };
        match snap.track().await {
            Some(hash) => ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"agent": alias, "hash": hash})),
                "snapshot auto-track captured tree"
            ),
            None => ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"agent": alias})),
                "snapshot auto-track failed"
            ),
        }
    }
}

async fn run_cleanup_pass(workspaces: &[(String, std::path::PathBuf)], data_dir: &Path) {
    for (alias, workspace) in workspaces {
        let Some(snap) = get_or_create(workspace, data_dir) else {
            continue;
        };
        snap.cleanup().await;
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"agent": alias})),
            "snapshot auto-cleanup pass complete"
        );
    }
}

/// Per-call entry point for explicit manual auto-tracks (e.g. from a test
/// harness or a `zeroclaw snapshot track --auto` CLI flag). Mirrors the
/// background pass without spawning a task.
#[allow(dead_code)]
pub async fn capture_all(config: &Arc<Config>) {
    let workspaces: Vec<(String, std::path::PathBuf)> = config
        .agents
        .keys()
        .map(|alias| (alias.clone(), config.agent_workspace_dir(alias)))
        .collect();
    run_capture_pass(&workspaces, &config.data_dir).await;
}
