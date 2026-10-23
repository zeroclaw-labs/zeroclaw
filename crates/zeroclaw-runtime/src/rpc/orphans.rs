//! Retirement of orphaned remote sessions.
//!
//! A remote client's sessions stay live after it disconnects, so that it can
//! reconnect with its identity and carry on. A client that never comes back,
//! or comes back under a new identity (which cannot reach them), would
//! otherwise hold its places in the daemon's live-session map until a
//! restart. Once the owning identity has had no live connection for
//! `wss.orphaned_session_timeout_secs`, each of its sessions that has been idle
//! as long, has no turn running and has no approval pending is closed, as
//! `session/close` would close it. Retirement never writes a transcript: a chat
//! session goes only when its store already holds all of its history, and one
//! whose store lacks part of it, differs from it or can't be read stays live.
//! Only the live session goes.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::context::RpcContext;
use super::session::OwnedSession;

/// How long retirement waits for a session's agent before leaving the session
/// for a later check.
const AGENT_WAIT: Duration = Duration::from_secs(5);

/// Close every orphaned remote session that is due at `now` and return the
/// ids closed. Also forgets the departures of identities that no longer own a
/// live session.
pub async fn retire_orphaned_sessions(ctx: &RpcContext, now: Instant) -> Vec<String> {
    retire_until_cancelled(ctx, now, &CancellationToken::new()).await
}

/// [`retire_orphaned_sessions`], stopping before the next session, and before
/// any session's removal, once `cancel` fires.
async fn retire_until_cancelled(
    ctx: &RpcContext,
    now: Instant,
    cancel: &CancellationToken,
) -> Vec<String> {
    let timeout_secs = ctx.config.read().wss.orphaned_session_timeout_secs;
    let checked_at = Instant::now();
    let owned = ctx.sessions.owned_sessions().await;
    let mut retired = Vec::new();
    if timeout_secs > 0 {
        let timeout = Duration::from_secs(timeout_secs);
        for candidate in &owned {
            if cancel.is_cancelled() {
                break;
            }
            if is_due(ctx, candidate, timeout, now)
                && retire(ctx, candidate, timeout, now, cancel).await
            {
                retired.push(candidate.id.clone());
            }
        }
    }
    if !retired.is_empty() {
        crate::util::release_freed_heap();
    }

    // Sessions are created only by a connected identity, so an identity that
    // departed before the snapshot has all its sessions in it. One that owns
    // none of them any more needs no departure record.
    let owners: HashSet<&str> = owned
        .iter()
        .filter(|session| !retired.contains(&session.id))
        .map(|session| session.owner_tui_id.as_str())
        .collect();
    ctx.tui_registry.retain_departures(|tui_id, departure| {
        departure.at >= checked_at || owners.contains(tui_id)
    });
    retired
}

/// Whether `session` is an orphaned remote session that is due at `now`.
fn is_due(ctx: &RpcContext, session: &OwnedSession, timeout: Duration, now: Instant) -> bool {
    is_due_with_departure(
        ctx,
        session,
        timeout,
        now,
        ctx.tui_registry.departure(&session.owner_tui_id),
    )
}

fn is_due_with_departure(
    ctx: &RpcContext,
    session: &OwnedSession,
    timeout: Duration,
    now: Instant,
    departure: Option<super::tui_identity::TuiDeparture>,
) -> bool {
    departure.is_some_and(|departure| {
        departure.remote && now.saturating_duration_since(departure.at) >= timeout
    }) && now.saturating_duration_since(session.last_active) >= timeout
        && !ctx.sessions.has_inflight_turn(&session.id)
        && !ctx.approval_pending.has_pending_for_session(&session.id)
}

/// Close one orphaned session, the way `session/close` closes a session.
async fn retire(
    ctx: &RpcContext,
    candidate: &OwnedSession,
    timeout: Duration,
    now: Instant,
    cancel: &CancellationToken,
) -> bool {
    // Serialize with prompts and client closes on the same session. The wait
    // is bounded by the queue's own lock timeout.
    let Ok(_permit) = ctx.sessions.session_queue.acquire(&candidate.id).await else {
        return false;
    };
    // The same incarnation, still idle and still orphaned, under the permit.
    let Some(current) = ctx.sessions.owned_session(&candidate.id).await else {
        return false;
    };
    if current.generation != candidate.generation
        || current.owner_tui_id != candidate.owner_tui_id
        || !is_due(ctx, &current, timeout, now)
        || !transcript_is_stored(ctx, &current).await
    {
        return false;
    }
    ctx.sessions.wait_test_retirement_pause().await;
    if cancel.is_cancelled() {
        return false;
    }
    // Decided and removed in one step: a resume, a recorded activity, a
    // reconnect or a cancellation after the checks above leaves the session
    // alone.
    let Some(agent) = ctx
        .sessions
        .remove_if_unchanged(&current, &ctx.tui_registry, |departure| {
            !cancel.is_cancelled() && is_due_with_departure(ctx, &current, timeout, now, departure)
        })
        .await
    else {
        return false;
    };
    // Only the agent detached here, never a successor's.
    agent
        .lock()
        .await
        .channel_handles()
        .unregister_channel("rpc");
    drop(agent);
    if let Some(ref hooks) = ctx.hooks {
        hooks.fire_session_end(&candidate.id, "rpc").await;
    }
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_category(::zeroclaw_log::EventCategory::Agent)
            .with_outcome(::zeroclaw_log::EventOutcome::Success)
            .with_attrs(::serde_json::json!({
                "session_id": candidate.id,
                "owner_tui_id": candidate.owner_tui_id,
                "timeout_secs": timeout.as_secs(),
            })),
        "Closed an orphaned remote session; its transcript is kept"
    );
    true
}

/// Whether the live history of `session` is already held by its durable
/// store. Nothing is written here.
///
/// An ACP session's turns each commit through its checkpoint, and a turn whose
/// checkpoint fails removes its live session, so a live ACP session's history
/// is already in the ACP store.
///
/// A chat session's post-turn write can fail, a daemon can run without a chat
/// store, and other writers (the gateway's message and delete routes) change
/// the stored transcript without the live session. So the stored transcript is
/// compared with the live one by role and content. Retirement goes ahead only
/// when the store holds everything live, alone or followed by messages another
/// writer added. A store that lacks part of the live history, differs from it,
/// or can't be read keeps the session live. Retirement writes nothing, so it
/// can't recreate a deleted session or reorder another writer's messages.
async fn transcript_is_stored(ctx: &RpcContext, session: &OwnedSession) -> bool {
    if matches!(
        ctx.sessions.chat_mode(&session.id).await,
        Some(crate::rpc::types::ChatMode::Acp)
    ) {
        return true;
    }
    let Some(agent) = ctx.sessions.get_agent(&session.id).await else {
        return false;
    };
    let Ok(agent) = tokio::time::timeout(AGENT_WAIT, agent.lock()).await else {
        return false;
    };
    let live = zeroclaw_providers::durable_chat_messages(agent.history());
    drop(agent);
    let Some(backend) = ctx.session_backend.as_ref() else {
        return live.is_empty();
    };
    let stored = match backend.try_load(&format!("rpc_{}", session.id)) {
        Ok(stored) => stored,
        Err(error) => return keep_live(session, "could not read its stored history", &error),
    };
    let held = stored.len() >= live.len()
        && stored
            .iter()
            .zip(&live)
            .all(|(stored, live)| stored.role == live.role && stored.content == live.content);
    if !held {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_category(::zeroclaw_log::EventCategory::Agent)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "session_id": session.id,
                    "stored_messages": stored.len(),
                    "live_messages": live.len(),
                })),
            "An orphaned session's store does not hold all of its history; keeping it live"
        );
    }
    held
}

fn keep_live(session: &OwnedSession, what: &str, error: &std::io::Error) -> bool {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Read)
            .with_category(::zeroclaw_log::EventCategory::Agent)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "session_id": session.id,
                "problem": what,
                "error": error.to_string(),
            })),
        "Keeping an orphaned session live: its history is not safely stored"
    );
    false
}

/// How long to wait between checks: the timeout itself when it is short,
/// otherwise a minute. With retirement off, a minute, so that turning it on
/// takes effect without a restart.
fn check_interval(timeout_secs: u64) -> Duration {
    if timeout_secs == 0 {
        return Duration::from_secs(60);
    }
    Duration::from_secs(timeout_secs.clamp(1, 60))
}

/// Run [`retire_orphaned_sessions`] until `cancel` fires or `ctx` is dropped.
/// The task holds `ctx` only while it checks, and a check stops at its next
/// safe point once `cancel` fires, so a finished daemon generation is not kept
/// alive or changed after it is told to stop.
pub fn spawn_orphan_retirement(
    ctx: &Arc<RpcContext>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let ctx = Arc::downgrade(ctx);
    zeroclaw_spawn::spawn!(async move {
        loop {
            let Some(timeout_secs) = ctx
                .upgrade()
                .map(|ctx| ctx.config.read().wss.orphaned_session_timeout_secs)
            else {
                return;
            };
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(check_interval(timeout_secs)) => {}
            }
            let Some(ctx) = ctx.upgrade() else {
                return;
            };
            retire_until_cancelled(&ctx, Instant::now(), &cancel).await;
            if cancel.is_cancelled() {
                return;
            }
        }
    })
}
