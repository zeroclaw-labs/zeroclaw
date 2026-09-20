//! Manual, recoverable context compaction for native ZeroCode Code
//! (ACP-mode) sessions.
//!
//! This module owns the manual operation's orchestration only. Durable
//! state, admission, and the live Agent each stay with their existing
//! owners: `AcpSessionStore` keeps originals, terminal ranges and the one
//! active derived checkpoint; `SessionActorQueue` provides fail-fast
//! idle-only admission; the Agent provides the routed provider call and the
//! history-projection install seam.
//!
//! Safety spine (the accepted contract):
//! - Originals are never replaced by the summary; the checkpoint is derived
//!   data with source identity, and restore returns to originals plus later
//!   turns without undoing any tool, file or external effect.
//! - Everything before the durable commit is best-effort: failure,
//!   cancellation, timeout, stale source or a refused result leaves the
//!   prior projection untouched. Everything after the commit treats the
//!   checkpoint as authoritative, even when the acknowledgement is lost.
//! - The summarization request is one bounded logical no-tool operation on
//!   the admitted Agent's existing routed provider under the canonical
//!   effective context budget. No tools, no delegation, no memory writes,
//!   no normal user prompt, and no `TurnComplete` (which could drain queued
//!   prompts).
//! - Durable work runs inside an owned settlement task that holds queue
//!   admission and the cancellation registration until the commit is joined
//!   and the live install/invalidation has settled. Destroying the response
//!   future (connection teardown aborts its task) cannot release admission
//!   while SQL or install is still in flight.

use super::dispatch::rpc_err;
use super::types::*;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use zeroclaw_api::jsonrpc::error_codes::*;
use zeroclaw_api::jsonrpc::{JsonRpcError, RpcOutbound};
use zeroclaw_api::model_provider::{ChatMessage, ConversationMessage};
use zeroclaw_infra::acp_session_store::AcpSessionStore;
use zeroclaw_infra::acp_session_store::{
    AcpActiveCheckpointRecord, AcpCheckpointOperationState, CompactionActivationError,
    CompactionActivationOutcome, CompactionActivationRequest, CompactionDeactivationError,
    CompactionDeactivationOutcome, CompactionSourceError, TerminalRangeKind,
    select_compaction_source,
};
use zeroclaw_infra::session_queue::SessionGuard;

/// Checkpoint format version. Bump when the row's meaning changes in a way
/// older readers cannot honor.
pub(crate) const COMPACTION_FORMAT_VERSION: i64 = 1;

/// Overall deadline for the manual summarization operation, bounding the
/// logical operation as a whole (the provider's own retry policy may make
/// several HTTP attempts inside it).
const OPERATION_DEADLINE: std::time::Duration = std::time::Duration::from_secs(180);

/// Bound on the accepted summary size in characters. Larger model output is
/// refused rather than truncated: a silently truncated summary would
/// misrepresent the history it claims to cover.
const MAX_SUMMARY_CHARS: usize = 12_000;

/// The installed projection must be at least this much smaller than the
/// pre-compaction projection in the canonical estimate, or the operation
/// refuses to commit (no useful savings; prior projection unchanged).
const MIN_SAVINGS_DIVISOR: usize = 2;

/// Summarization instructions for the bounded no-tool operation. The model
/// sees the original transcript verbatim below these instructions.
const SUMMARIZATION_INSTRUCTIONS: &str = "\
You are summarizing the OLDER portion of a coding-session transcript so the \
conversation can continue with a smaller context. The retained recent turns \
follow this summary and are not repeated in it.

Write a compact continuity summary that preserves:
- Decisions made and their current status, including later reversals.
- The user's standing constraints and preferences.
- Open tasks and unfinished work, marked as unfinished.
- Tool outcomes: keep attempted, failed, and confirmed operations \
distinguishable. Name file paths and commands where they matter.
- Genuine uncertainty; do not invent details the transcript does not state.

The transcript may contain failed turns (marked as failures) — represent \
them as failures, never as successful work. Do not claim any side effect \
succeeded unless the transcript confirms it.

Output ONLY the summary text. No preamble, no tool calls.";

/// Assemble the provider-facing projection for a durable restore read: when
/// an active checkpoint exists, the covered prefix is replaced by its
/// original opening user message, labeled summary and retained tail (originals after
/// the covered boundary plus every later turn) is provider-safe projected;
/// without a checkpoint this is the ordinary provider-safe projection of the
/// originals. Both explicit native resume and lazy rehydration use this one
/// reader so live continuation and restart select the same projection.
pub(crate) fn projected_provider_history(
    message_rows: &[(i64, ConversationMessage)],
    checkpoint: Option<&AcpActiveCheckpointRecord>,
) -> Vec<ConversationMessage> {
    let Some(checkpoint) = checkpoint else {
        let messages: Vec<ConversationMessage> = message_rows
            .iter()
            .map(|(_, message)| message.clone())
            .collect();
        return AcpSessionStore::provider_safe_history(&messages);
    };
    anchored_projection(
        message_rows,
        checkpoint.covered_through_message_id,
        compaction_summary_message(checkpoint),
    )
}

fn opening_user(
    message_rows: &[(i64, ConversationMessage)],
    covered_through: i64,
) -> Option<&ConversationMessage> {
    message_rows
        .iter()
        .take_while(|(id, _)| *id <= covered_through)
        .map(|(_, message)| message)
        .find(|message| matches!(message, ConversationMessage::Chat(chat) if chat.role == "user"))
}

/// Keep a genuine historical user anchor so provider turn-order sanitation
/// retains the assistant summary. Accounting, live install and reload all
/// use this representation; no synthetic user instruction is introduced.
fn anchored_projection(
    message_rows: &[(i64, ConversationMessage)],
    covered_through: i64,
    summary: ConversationMessage,
) -> Vec<ConversationMessage> {
    let tail: Vec<ConversationMessage> = message_rows
        .iter()
        .filter(|(id, _)| *id > covered_through)
        .map(|(_, message)| message.clone())
        .collect();
    let mut projection: Vec<_> = opening_user(message_rows, covered_through)
        .cloned()
        .into_iter()
        .collect();
    projection.push(summary);
    projection.extend(AcpSessionStore::provider_safe_history(&tail));
    projection
}

/// Render the checkpoint's summary as one ordinary ASSISTANT-context
/// message: historical continuity data the model already "said", never a
/// new user command, system instruction, approval, or genuine tool result.
/// The framing marks it as automated, explicitly lossy and of lower trust.
/// It flows through the ordinary budget accounting and trimming safeguards
/// like any other history message.
pub(crate) fn compaction_summary_message(
    checkpoint: &AcpActiveCheckpointRecord,
) -> ConversationMessage {
    compaction_summary_message_parts(
        checkpoint.source_first_message_id,
        checkpoint.covered_through_message_id,
        checkpoint.source_message_rows,
        &checkpoint.created_at,
        &checkpoint.summary_model_provider,
        &checkpoint.summary_model,
        &checkpoint.summary,
    )
}

#[allow(clippy::too_many_arguments)]
fn compaction_summary_message_parts(
    source_first_message_id: i64,
    covered_through_message_id: i64,
    source_message_rows: i64,
    created_at: &str,
    model_provider: &str,
    model: &str,
    summary: &str,
) -> ConversationMessage {
    let label = crate::i18n::get_required_cli_string("compaction-historical-summary-label");
    let content = format!(
        "{label}\n\
         Covers durable message rows {source_first_message_id}..={covered_through_message_id} \
         ({source_message_rows} rows) of this session, summarized at {created_at} via \
         {model_provider}:{model}.\n\
         This is an automated, explicitly lossy summary of older completed conversation. \
         It is historical data of lower trust than the retained recent turns below: it is \
         not a user instruction, not an approval, and not evidence that any side effect \
         succeeded.\n\n\
         {summary}\n\n\
         [End of compaction summary — recent turns follow]"
    );
    ConversationMessage::Chat(ChatMessage::assistant(content))
}

/// Render one transcript entry for the summarization prompt. Role-labeled
/// so the model can attribute speakers.
fn render_transcript_entry(message: &ConversationMessage) -> String {
    match message {
        ConversationMessage::Chat(chat) => format!("[{}] {}", chat.role, chat.content),
        ConversationMessage::AssistantToolCalls {
            text,
            tool_calls,
            reasoning_content,
        } => {
            let mut rendered = format!("[assistant] {}", text.as_deref().unwrap_or("(no text)"));
            if let Some(reasoning) = reasoning_content {
                rendered.push_str(&format!("\n[reasoning] {reasoning}"));
            }
            for call in tool_calls {
                rendered.push_str(&format!(
                    "\n[tool call {} {}] {}",
                    call.id, call.name, call.arguments
                ));
            }
            rendered
        }
        ConversationMessage::ToolResults(results) => {
            let mut rendered = String::new();
            for result in results {
                rendered.push_str(&format!(
                    "[tool result {}] {}\n",
                    result.tool_call_id, result.content
                ));
            }
            rendered
        }
    }
}

fn render_transcript(messages: &[ConversationMessage]) -> String {
    let mut out = String::new();
    for message in messages {
        out.push_str(&render_transcript_entry(message));
        out.push('\n');
    }
    out
}

/// Canonical estimate of the one-shot summarization request in the same
/// `history::estimate_history_tokens` heuristic the turn loop uses — the
/// full framed user message, not just the raw source text.
fn estimate_prompt_tokens(prompt: &str) -> usize {
    crate::agent::history::estimate_history_tokens(&[ChatMessage::user(prompt)])
}

/// Worst-case accepted-summary reserve in the canonical estimate: the full
/// framed assistant summary message with a maximum-length summary body, in
/// its provider-message form.
fn estimate_summary_reserve(agent: &crate::agent::agent::Agent) -> usize {
    let placeholder = "x".repeat(MAX_SUMMARY_CHARS);
    let worst_case = compaction_summary_message_parts(
        1,
        1,
        1,
        "1970-01-01T00:00:00Z",
        "provider",
        "model",
        &placeholder,
    );
    agent.estimate_provider_messages(&[worst_case])
}

/// Everything the handlers need about the admitted live session, captured
/// while holding queue admission so nothing can replace it mid-operation.
struct AdmittedSession {
    generation: u64,
    agent: Arc<tokio::sync::Mutex<crate::agent::agent::Agent>>,
}

/// Fail-fast idle-only admission plus the existing session-ownership and
/// surface authorization checks. Busy is a typed result — compaction never
/// queues behind a running turn and never cancels one.
async fn admit_session(
    ctx: &Arc<super::context::RpcContext>,
    caller_tui_id: Option<&str>,
    session_id: &str,
) -> Result<(SessionGuard, AdmittedSession), JsonRpcError> {
    // Ownership check first, mirroring session/cancel: a foreign TUI must
    // not even contend for admission.
    let owner = ctx.sessions.session_owner_tui_id(session_id).await;
    let allowed = match (owner.as_ref().and_then(|o| o.as_deref()), caller_tui_id) {
        (Some(owner_id), Some(caller_id)) => owner_id == caller_id,
        _ => false,
    };
    if !allowed {
        return Err(rpc_err(
            SESSION_NOT_OWNED,
            "Caller does not own this session",
        ));
    }
    if ctx.sessions.chat_mode(session_id).await != Some(ChatMode::Acp) {
        return Err(rpc_err(
            INVALID_PARAMS,
            "Context compaction is only available for native Code (ACP) sessions",
        ));
    }
    if ctx.sessions.interaction_surface(session_id).await.flatten()
        != Some(crate::agent::prompt::InteractionSurface::ZerocodeCode)
    {
        return Err(rpc_err(
            INVALID_PARAMS,
            "Context compaction is only available for the native ZeroCode Code surface",
        ));
    }

    let guard = ctx
        .sessions
        .session_queue
        .try_acquire_idle(session_id)
        .await
        .ok_or_else(|| {
            rpc_err(
                SESSION_BUSY,
                "Session is busy with a running or queued turn; try compacting context \
                 when it is idle",
            )
        })?;

    // A resume may have rebound ownership while we acquired admission.
    let owner = ctx.sessions.session_owner_tui_id(session_id).await;
    if !matches!(
        (owner.as_ref().and_then(|o| o.as_deref()), caller_tui_id),
        (Some(owner_id), Some(caller_id)) if owner_id == caller_id
    ) {
        return Err(rpc_err(
            SESSION_NOT_OWNED,
            "Caller does not own this session",
        ));
    }

    // Re-validate under admission: the incarnation admitted here is the one
    // the operation runs against, and replacement can no longer race it.
    if ctx.sessions.chat_mode(session_id).await != Some(ChatMode::Acp) {
        return Err(rpc_err(
            INVALID_PARAMS,
            "Context compaction is only available for native Code (ACP) sessions",
        ));
    }
    if ctx.sessions.interaction_surface(session_id).await.flatten()
        != Some(crate::agent::prompt::InteractionSurface::ZerocodeCode)
    {
        return Err(rpc_err(
            INVALID_PARAMS,
            "Context compaction is only available for the native ZeroCode Code surface",
        ));
    }
    let generation = ctx
        .sessions
        .get_generation(session_id)
        .await
        .ok_or_else(|| rpc_err(SESSION_NOT_FOUND, "Session not found"))?;
    let agent = ctx.sessions.get_agent(session_id).await.ok_or_else(|| {
        rpc_err(
            SESSION_NOT_FOUND,
            "Session has no live agent; reopen the session before compacting context",
        )
    })?;
    Ok((guard, AdmittedSession { generation, agent }))
}

/// Read the durable snapshot for one operation (single SQLite read
/// snapshot) and apply the compaction eligibility gates that belong to the
/// durable world: existence, kill state, native surface, and no pending
/// interrupted turn.
async fn read_snapshot(
    store: &Arc<AcpSessionStore>,
    session_id: &str,
    operation_id: &str,
) -> Result<zeroclaw_infra::acp_session_store::AcpCompactionSnapshot, JsonRpcError> {
    let store = Arc::clone(store);
    let sid = session_id.to_string();
    let op = operation_id.to_string();
    let snapshot = tokio::task::spawn_blocking(move || store.read_compaction_snapshot(&sid, &op))
        .await
        .map_err(|join| {
            rpc_err(
                INTERNAL_ERROR,
                format!("Compaction snapshot task failed: {join}"),
            )
        })?
        .map_err(|error| {
            rpc_err(
                INTERNAL_ERROR,
                format!("Failed to read compaction snapshot: {error}"),
            )
        })?;
    let Some(snapshot) = snapshot else {
        return Err(rpc_err(SESSION_NOT_FOUND, "Session not found"));
    };
    if snapshot.killed {
        return Err(rpc_err(SESSION_NOT_FOUND, "Session not found"));
    }
    if snapshot.interaction_surface.as_deref() != Some("zerocode_code") {
        return Err(rpc_err(
            INVALID_PARAMS,
            "Context compaction is only available for the native ZeroCode Code surface",
        ));
    }
    if snapshot.inflight_turn_id.is_some() {
        return Err(rpc_err(
            SESSION_BUSY,
            "Session has an interrupted turn pending recovery; reopen the session before \
             compacting context",
        ));
    }
    Ok(snapshot)
}

fn source_refusal(error: CompactionSourceError) -> JsonRpcError {
    rpc_err(INVALID_PARAMS, format!("Cannot compact context: {error}"))
}

fn summarization_failure(error: crate::agent::agent::BoundedSummarizationError) -> JsonRpcError {
    match error {
        crate::agent::agent::BoundedSummarizationError::Cancelled => rpc_err(
            SESSION_BUSY,
            "Context compaction was cancelled before commit; the prior projection is unchanged",
        ),
        other => rpc_err(
            INTERNAL_ERROR,
            format!("Context compaction summarization failed unchanged: {other}"),
        ),
    }
}

fn activation_failure(error: CompactionActivationError) -> JsonRpcError {
    match error {
        CompactionActivationError::SessionMissing
        | CompactionActivationError::IncarnationMismatch { .. } => rpc_err(
            SESSION_NOT_FOUND,
            "Session changed during compaction; nothing was committed",
        ),
        CompactionActivationError::SessionKilled => rpc_err(
            SESSION_NOT_FOUND,
            "Session was killed during compaction; nothing was committed",
        ),
        CompactionActivationError::InflightTurn => rpc_err(
            SESSION_BUSY,
            "A turn was in flight during compaction; nothing was committed",
        ),
        CompactionActivationError::SourceMismatch { detail } => rpc_err(
            INVALID_PARAMS,
            format!("Compaction source changed before commit: {detail}"),
        ),
        CompactionActivationError::StaleActiveCheckpoint { .. } => rpc_err(
            INVALID_PARAMS,
            "The active compaction changed since this request started; retry with a fresh \
             operation",
        ),
        CompactionActivationError::Storage(detail) => rpc_err(
            INTERNAL_ERROR,
            format!("Compaction commit failed; nothing was changed: {detail}"),
        ),
    }
}

fn deactivation_failure(error: CompactionDeactivationError) -> JsonRpcError {
    match error {
        CompactionDeactivationError::SessionMissing
        | CompactionDeactivationError::IncarnationMismatch { .. } => rpc_err(
            SESSION_NOT_FOUND,
            "Session changed during restore; nothing was changed",
        ),
        CompactionDeactivationError::SessionKilled => {
            rpc_err(SESSION_NOT_FOUND, "Session is killed; nothing was changed")
        }
        CompactionDeactivationError::InflightTurn => rpc_err(
            SESSION_BUSY,
            "A turn was in flight during restore; nothing was changed",
        ),
        CompactionDeactivationError::StaleActiveCheckpoint { .. } => rpc_err(
            INVALID_PARAMS,
            "The active compaction changed since this request started; retry with a fresh \
             operation",
        ),
        CompactionDeactivationError::Storage(detail) => rpc_err(
            INTERNAL_ERROR,
            format!("Restore failed; nothing was changed: {detail}"),
        ),
    }
}

/// Forward an install-time structured-cap trim event to the client so the
/// visible-omission contract stays identical to seeded restores.
async fn forward_trim_event(
    rpc: &Arc<RpcOutbound>,
    session_id: &str,
    event: Option<crate::agent::agent::TurnEvent>,
) {
    if let Some(event) = event
        && let Some(notification) =
            super::dispatch::notification_for_turn_event(session_id, &event, None, None)
    {
        let _ = rpc.send_raw(notification).await;
    }
}

/// Install a projection onto the admitted live Agent, validated against the
/// history captured under admission. A mismatch (or an unrecoverable
/// system-prompt failure) invalidates the EXACT live incarnation — the
/// generation captured under admission, never a successor that happens to
/// reuse the public session id — so the next prompt rehydrates from the
/// committed projection instead of mixing generations.
async fn install_projection(
    ctx: &Arc<super::context::RpcContext>,
    rpc: &Arc<RpcOutbound>,
    session_id: &str,
    agent: &Arc<tokio::sync::Mutex<crate::agent::agent::Agent>>,
    expected_history: &[ConversationMessage],
    projection: Vec<ConversationMessage>,
    expected_generation: u64,
) -> bool {
    let install = agent
        .lock()
        .await
        .install_conversation_history_projection(expected_history, projection);
    match install {
        crate::agent::agent::HistoryProjectionInstall::Installed { trim_event } => {
            forward_trim_event(rpc, session_id, trim_event).await;
            true
        }
        crate::agent::agent::HistoryProjectionInstall::LiveHistoryMismatch => {
            invalidate_live_incarnation(ctx, session_id, expected_generation).await;
            false
        }
        crate::agent::agent::HistoryProjectionInstall::SystemPromptFailed => {
            invalidate_live_incarnation(ctx, session_id, expected_generation).await;
            false
        }
    }
}

/// Remove the live incarnation only while it is still the exact generation
/// the operation admitted; a successor reusing the public session id must
/// never be removed by UUID alone.
async fn invalidate_live_incarnation(
    ctx: &Arc<super::context::RpcContext>,
    session_id: &str,
    expected_generation: u64,
) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
            .with_category(::zeroclaw_log::EventCategory::Agent)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "session_id": session_id,
                "expected_generation": expected_generation,
            })),
        "Compaction install could not replace the live projection; invalidating the exact \
         admitted incarnation so the next prompt rehydrates from the committed projection"
    );
    if ctx.sessions.get_generation(session_id).await == Some(expected_generation) {
        ctx.sessions.remove(session_id).await;
    }
}

/// Manual `/compact-context` operation. See the module docs for the safety
/// spine; the durable commit is the authority boundary.
///
/// The durable work runs inside an owned settlement task holding queue
/// admission and the cancellation registration: destroying this response
/// future (connection teardown aborts the dispatcher task that owns it)
/// cannot release admission while the commit or install is in flight. The
/// connection and session cancellation tokens both reach the bounded
/// provider operation, so pre-commit model work still stops on teardown.
pub(crate) async fn compact_context(
    ctx: &Arc<super::context::RpcContext>,
    rpc: &Arc<RpcOutbound>,
    caller_tui_id: Option<&str>,
    connection_cancel: &CancellationToken,
    params: SessionCompactContextParams,
) -> Result<SessionCompactContextResult, JsonRpcError> {
    let store = ctx
        .acp_session_store
        .clone()
        .ok_or_else(|| rpc_err(INTERNAL_ERROR, "ACP session store is not available"))?;
    let session_id = params.session_id;
    let operation_id = params
        .operation_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let (guard, admitted) = admit_session(ctx, caller_tui_id, &session_id).await?;
    if connection_cancel.is_cancelled() {
        return Err(rpc_err(
            SESSION_BUSY,
            "RPC connection closed before compaction ran",
        ));
    }

    // Cancellation registration with the existing session mechanisms: user
    // cancel, close, kill and connection teardown can all signal this
    // operation, and removal/kill handlers can target the exact incarnation.
    let cancel = CancellationToken::new();
    let cancel_registration = Arc::clone(&ctx.sessions).register_operation_cancel_token(
        &session_id,
        Some(admitted.generation),
        cancel.clone(),
    );

    // Settlement task: owns admission, the cancellation registration, the
    // context and the store handles until the durable commit is joined and
    // the live install/invalidation has settled. Its handle is deliberately
    // NOT registered with the connection's abortable prompt tasks — the
    // database write is never detached from its guard and reconciliation
    // owner, which is this task.
    let (result_tx, result_rx) = oneshot::channel();
    let settlement_ctx = Arc::clone(ctx);
    let settlement_rpc = Arc::clone(rpc);
    let settlement_store = Arc::clone(&store);
    let settlement_session = session_id.clone();
    let settlement_operation = operation_id.clone();
    let settlement_connection_cancel = connection_cancel.clone();
    zeroclaw_spawn::spawn!(async move {
        // Held in THIS task: aborting the response future cannot release it.
        let _admission = guard;
        let result = run_compact(
            &settlement_ctx,
            &settlement_rpc,
            &settlement_store,
            &admitted,
            &settlement_session,
            &settlement_operation,
            &cancel,
            &settlement_connection_cancel,
        )
        .await;
        let _cancel_cause = cancel_registration.finish();
        let _ = result_tx.send(result);
    });

    match result_rx.await {
        Ok(result) => result,
        Err(_response_future_dropped) => Err(rpc_err(
            SESSION_BUSY,
            "Context compaction was detached from this connection before completion; it \
             settles in the background and the session stays reserved until then",
        )),
    }
}

async fn run_compact(
    ctx: &Arc<super::context::RpcContext>,
    rpc: &Arc<RpcOutbound>,
    store: &Arc<AcpSessionStore>,
    admitted: &AdmittedSession,
    session_id: &str,
    operation_id: &str,
    cancel: &CancellationToken,
    connection_cancel: &CancellationToken,
) -> Result<SessionCompactContextResult, JsonRpcError> {
    // Live projection captured under admission; the install seam validates
    // against exactly this capture.
    let expected_history = admitted.agent.lock().await.history().to_vec();

    let snapshot = read_snapshot(store, session_id, operation_id).await?;
    let session_row_id = snapshot.session_row_id;

    // Committed retry of the ACTIVE checkpoint: recognized idempotently,
    // with no second model operation and no write. The live projection is
    // NOT assumed to be installed already (the acknowledgement may have
    // been lost before install): reconcile or invalidate below.
    if let Some(active) = snapshot.active_checkpoint.as_ref()
        && active.operation_id == operation_id
    {
        let (checkpoint, installed) = install_committed_projection(
            ctx,
            rpc,
            store,
            admitted,
            session_id,
            operation_id,
            &expected_history,
        )
        .await?;
        return Ok(compact_result_from_checkpoint(
            session_id,
            operation_id,
            "already_committed",
            &checkpoint,
            &snapshot,
            admitted,
            installed,
        )
        .await);
    }

    // This operation committed earlier but a later restore or recompaction
    // superseded it: report the supersession without touching the current
    // projection. An old compact retry must never undo a later restore.
    if snapshot.operation_checkpoint == Some(AcpCheckpointOperationState::Inactive) {
        return Ok(SessionCompactContextResult {
            session_id: session_id.to_string(),
            operation_id: operation_id.to_string(),
            status: "superseded".to_string(),
            covered_turns: 0,
            covered_message_rows: 0,
            estimated_tokens_before: 0,
            estimated_tokens_after: 0,
            summary: String::new(),
            model_provider: String::new(),
            model: String::new(),
            usage: None,
            installed: false,
        });
    }

    // Fresh compaction (or recompaction over an active checkpoint from a
    // different operation): coverage is recomputed from ORIGINALS, never
    // from a previous summary.
    let selection = select_compaction_source(&snapshot.message_rows, &snapshot.terminal_ranges)
        .map_err(source_refusal)?;
    if opening_user(&snapshot.message_rows, selection.covered_through_message_id).is_none() {
        return Err(rpc_err(
            INVALID_PARAMS,
            "Cannot compact context: the covered history has no opening user message",
        ));
    }
    let covered: Vec<ConversationMessage> = snapshot
        .message_rows
        .iter()
        .take_while(|(id, _)| *id <= selection.covered_through_message_id)
        .map(|(_, message)| message.clone())
        .collect();

    // Input bound against the canonical effective context budget, in the
    // canonical estimate over the ACTUAL one-shot request message (full
    // framing included), plus the worst-case accepted-summary reserve.
    let prompt = format!(
        "{SUMMARIZATION_INSTRUCTIONS}\n\n{}",
        render_transcript(&covered)
    );
    let (budget, summary_reserve, model_provider, model) = {
        let agent = admitted.agent.lock().await;
        let attribution = agent.attribution_fields();
        (
            agent.effective_context_budget(),
            estimate_summary_reserve(&agent),
            attribution.1,
            attribution.2,
        )
    };
    let prompt_tokens = estimate_prompt_tokens(&prompt);
    if prompt_tokens + summary_reserve > budget {
        return Err(rpc_err(
            INVALID_PARAMS,
            format!(
                "Cannot compact context: the selected source (~{prompt_tokens} estimated \
                 tokens) does not fit the effective context budget ({budget}) for one \
                 bounded summarization operation"
            ),
        ));
    }

    // One bounded logical no-tool operation on the admitted Agent's existing
    // routed provider and model. The CONNECTION token participates in the
    // same select as the provider call — teardown stops the model work, not
    // just the checks around it.
    let summarization = {
        let agent = admitted.agent.lock().await;
        let call =
            agent.run_bounded_summarization(&prompt, MAX_SUMMARY_CHARS, OPERATION_DEADLINE, cancel);
        tokio::select! {
            biased;
            _ = connection_cancel.cancelled() => Err(
                crate::agent::agent::BoundedSummarizationError::Cancelled,
            ),
            result = call => result,
        }
    }
    .map_err(summarization_failure)?;
    let summary = summarization.summary;

    // Refuse results without useful savings, unchanged, in the canonical
    // estimate over the actual provider-message projections (framing
    // included on both sides).
    let (before_estimate, after_estimate) = {
        let agent = admitted.agent.lock().await;
        // Compare with the admitted live history, which may already have
        // trimmed the checkpoint projection. Originals remain summary input.
        // Normalize both sides without the separately rebuilt system prompt.
        let before_messages = AcpSessionStore::provider_safe_history(&expected_history);
        let after_messages = projected_provider_history_from_summary(
            &snapshot.message_rows,
            &selection,
            &summary,
            &model_provider,
            &model,
        );
        if !agent.conversation_history_projection_fits(&after_messages) {
            return Err(rpc_err(
                INVALID_PARAMS,
                "Cannot compact context: the summary and retained tail exceed the live message limit; nothing was changed",
            ));
        }
        (
            agent.estimate_provider_messages(&before_messages),
            agent.estimate_provider_messages(&after_messages),
        )
    };
    if after_estimate.saturating_mul(MIN_SAVINGS_DIVISOR) >= before_estimate {
        return Err(rpc_err(
            INVALID_PARAMS,
            "Context compaction refused: the generated summary (including its provenance \
             framing) would not reduce context usefully; nothing was changed",
        ));
    }

    if cancel.is_cancelled() || connection_cancel.is_cancelled() {
        return Err(rpc_err(
            SESSION_BUSY,
            "Context compaction was cancelled before commit; the prior projection is \
             unchanged",
        ));
    }

    // Durable commit: one write transaction rechecking incarnation, kill
    // state, in-flight turns, exact source identity, and the prior active
    // operation this request snapshotted (a stale request must never
    // supersede a later operation). The blocking task is joined — never
    // detached — inside this settlement task, which owns admission.
    let commit_session = session_id.to_string();
    let commit_operation = operation_id.to_string();
    let commit_summary = summary.clone();
    let commit_model_provider = model_provider.clone();
    let commit_model = model.clone();
    let commit_input_tokens = summarization
        .usage
        .as_ref()
        .and_then(|usage| usage.input_tokens);
    let commit_output_tokens = summarization
        .usage
        .as_ref()
        .and_then(|usage| usage.output_tokens);
    let expected_prior_active_operation = snapshot
        .active_checkpoint
        .as_ref()
        .map(|checkpoint| checkpoint.operation_id.clone());
    let store_for_commit = Arc::clone(store);
    let activation = tokio::task::spawn_blocking(move || {
        let request = CompactionActivationRequest {
            session_uuid: &commit_session,
            expected_session_row_id: session_row_id,
            format_version: COMPACTION_FORMAT_VERSION,
            operation_id: &commit_operation,
            expected_prior_active_operation: expected_prior_active_operation.as_deref(),
            source_first_message_id: selection.first_message_id,
            covered_through_message_id: selection.covered_through_message_id,
            source_message_rows: selection.covered_message_rows as i64,
            summary: &commit_summary,
            summary_model_provider: &commit_model_provider,
            summary_model: &commit_model,
            input_tokens: commit_input_tokens,
            output_tokens: commit_output_tokens,
        };
        store_for_commit.activate_compaction_checkpoint(&request)
    })
    .await
    .map_err(|join| {
        rpc_err(
            INTERNAL_ERROR,
            format!("Compaction commit task failed: {join}"),
        )
    })?
    .map_err(activation_failure)?;

    // Post-commit: the checkpoint is authoritative even if the
    // acknowledgement is lost or cancellation fires from here on. The
    // installed projection is read back from the committed checkpoint —
    // one durable projection for success, retry and reload — and installed
    // by replacing the live projection (or invalidating the exact
    // incarnation on mismatch).
    let (checkpoint, installed) = match activation {
        CompactionActivationOutcome::Activated => {
            install_committed_projection(
                ctx,
                rpc,
                store,
                admitted,
                session_id,
                operation_id,
                &expected_history,
            )
            .await?
        }
        CompactionActivationOutcome::AlreadyActive => {
            // A concurrent duplicate committed the same operation; its
            // checkpoint is authoritative. Reconcile or invalidate rather
            // than assuming the live state already carries it.
            let (checkpoint, installed) = install_committed_projection(
                ctx,
                rpc,
                store,
                admitted,
                session_id,
                operation_id,
                &expected_history,
            )
            .await?;
            let snapshot = read_snapshot(store, session_id, operation_id).await?;
            return Ok(compact_result_from_checkpoint(
                session_id,
                operation_id,
                "already_committed",
                &checkpoint,
                &snapshot,
                admitted,
                installed,
            )
            .await);
        }
    };

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
            .with_category(::zeroclaw_log::EventCategory::Agent)
            .with_outcome(if installed {
                ::zeroclaw_log::EventOutcome::Success
            } else {
                ::zeroclaw_log::EventOutcome::Unknown
            })
            .with_attrs(::serde_json::json!({
                "session_id": session_id,
                "operation_id": operation_id,
                "covered_ranges": selection.covered_ranges,
                "covered_message_rows": selection.covered_message_rows,
                "installed": installed,
            })),
        "Manual context compaction committed"
    );

    Ok(SessionCompactContextResult {
        session_id: session_id.to_string(),
        operation_id: operation_id.to_string(),
        status: "activated".to_string(),
        covered_turns: selection.covered_ranges,
        covered_message_rows: selection.covered_message_rows,
        estimated_tokens_before: before_estimate as u64,
        estimated_tokens_after: after_estimate as u64,
        summary,
        model_provider: checkpoint.summary_model_provider.clone(),
        model: checkpoint.summary_model.clone(),
        usage: summarization.usage.map(|usage| CompactionUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
        }),
        installed,
    })
}

/// The post-compaction projection in the exact form the committed checkpoint
/// will reproduce on reload: the labeled assistant summary carrying the
/// operation's provenance, followed by the provider-safe tail. Used for the
/// pre-commit savings comparison so the accepted form is what is measured.
fn projected_provider_history_from_summary(
    message_rows: &[(i64, ConversationMessage)],
    selection: &zeroclaw_infra::acp_session_store::CompactionSourceSelection,
    summary: &str,
    model_provider: &str,
    model: &str,
) -> Vec<ConversationMessage> {
    let summary_message = compaction_summary_message_parts(
        selection.first_message_id,
        selection.covered_through_message_id,
        selection.covered_message_rows as i64,
        "pending-commit",
        model_provider,
        model,
        summary,
    );
    anchored_projection(
        message_rows,
        selection.covered_through_message_id,
        summary_message,
    )
}

/// Read the committed projection for `operation_id` from a fresh durable
/// snapshot and install it onto the admitted live Agent — one durable
/// projection for success, committed retry and reload. Returns the
/// committed checkpoint and whether the live incarnation still carries it
/// (false means the exact incarnation was invalidated and the next prompt
/// rehydrates from the committed checkpoint).
async fn install_committed_projection(
    ctx: &Arc<super::context::RpcContext>,
    rpc: &Arc<RpcOutbound>,
    store: &Arc<AcpSessionStore>,
    admitted: &AdmittedSession,
    session_id: &str,
    operation_id: &str,
    expected_history: &[ConversationMessage],
) -> Result<(AcpActiveCheckpointRecord, bool), JsonRpcError> {
    let snapshot = match read_snapshot(store, session_id, operation_id).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            invalidate_live_incarnation(ctx, session_id, admitted.generation).await;
            return Err(error);
        }
    };
    let Some(checkpoint) = snapshot
        .active_checkpoint
        .clone()
        .filter(|checkpoint| checkpoint.operation_id == operation_id)
    else {
        invalidate_live_incarnation(ctx, session_id, admitted.generation).await;
        return Err(rpc_err(
            INTERNAL_ERROR,
            "The committed compaction checkpoint could not be confirmed; nothing was installed",
        ));
    };
    let projection = projected_provider_history(&snapshot.message_rows, Some(&checkpoint));
    let installed = install_projection(
        ctx,
        rpc,
        session_id,
        &admitted.agent,
        expected_history,
        projection,
        admitted.generation,
    )
    .await;
    Ok((checkpoint, installed))
}

/// Build the acknowledgement for a committed checkpoint from the stored row
/// and the current snapshot, with display estimates recomputed in the
/// canonical estimate over the actual provider-message projections.
async fn compact_result_from_checkpoint(
    session_id: &str,
    operation_id: &str,
    status: &str,
    checkpoint: &AcpActiveCheckpointRecord,
    snapshot: &zeroclaw_infra::acp_session_store::AcpCompactionSnapshot,
    admitted: &AdmittedSession,
    installed: bool,
) -> SessionCompactContextResult {
    let covered_turns = snapshot
        .terminal_ranges
        .iter()
        .filter(|range| {
            range.last_message_id <= checkpoint.covered_through_message_id
                && range.kind == TerminalRangeKind::Completed
        })
        .count();
    let covered: Vec<ConversationMessage> = snapshot
        .message_rows
        .iter()
        .take_while(|(id, _)| *id <= checkpoint.covered_through_message_id)
        .map(|(_, message)| message.clone())
        .collect();
    let (before_estimate, after_estimate) = {
        let agent = admitted.agent.lock().await;
        let before_messages = {
            let mut messages = covered;
            messages.extend(
                snapshot
                    .message_rows
                    .iter()
                    .filter(|(id, _)| *id > checkpoint.covered_through_message_id)
                    .map(|(_, message)| message.clone()),
            );
            AcpSessionStore::provider_safe_history(&messages)
        };
        let after_messages = projected_provider_history(&snapshot.message_rows, Some(checkpoint));
        (
            agent.estimate_provider_messages(&before_messages),
            agent.estimate_provider_messages(&after_messages),
        )
    };
    SessionCompactContextResult {
        session_id: session_id.to_string(),
        operation_id: operation_id.to_string(),
        status: status.to_string(),
        covered_turns,
        covered_message_rows: checkpoint.source_message_rows.max(0) as usize,
        estimated_tokens_before: before_estimate as u64,
        estimated_tokens_after: after_estimate as u64,
        summary: checkpoint.summary.clone(),
        model_provider: checkpoint.summary_model_provider.clone(),
        model: checkpoint.summary_model.clone(),
        usage: Some(CompactionUsage {
            input_tokens: checkpoint.input_tokens,
            output_tokens: checkpoint.output_tokens,
        }),
        installed,
    }
}

/// Manual `/restore-context` operation: deactivate the active checkpoint
/// and rebuild the live projection from retained originals plus subsequent
/// turns under the existing visible limits. Restore never rewinds the
/// conversation, reruns tools, or reverses external effects. Like compact,
/// the durable deactivation and live install settle inside an owned
/// settlement task holding admission.
pub(crate) async fn restore_context(
    ctx: &Arc<super::context::RpcContext>,
    rpc: &Arc<RpcOutbound>,
    caller_tui_id: Option<&str>,
    connection_cancel: &CancellationToken,
    params: SessionRestoreContextParams,
) -> Result<SessionRestoreContextResult, JsonRpcError> {
    let store = ctx
        .acp_session_store
        .clone()
        .ok_or_else(|| rpc_err(INTERNAL_ERROR, "ACP session store is not available"))?;
    let session_id = params.session_id;
    let operation_id = params
        .operation_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let (guard, admitted) = admit_session(ctx, caller_tui_id, &session_id).await?;
    if connection_cancel.is_cancelled() {
        return Err(rpc_err(
            SESSION_BUSY,
            "RPC connection closed before restore ran",
        ));
    }

    let cancel = CancellationToken::new();
    let cancel_registration = Arc::clone(&ctx.sessions).register_operation_cancel_token(
        &session_id,
        Some(admitted.generation),
        cancel.clone(),
    );

    let (result_tx, result_rx) = oneshot::channel();
    let settlement_ctx = Arc::clone(ctx);
    let settlement_rpc = Arc::clone(rpc);
    let settlement_store = Arc::clone(&store);
    let settlement_session = session_id.clone();
    let settlement_operation = operation_id.clone();
    let settlement_connection_cancel = connection_cancel.clone();
    zeroclaw_spawn::spawn!(async move {
        let _admission = guard;
        let result = run_restore(
            &settlement_ctx,
            &settlement_rpc,
            &settlement_store,
            &admitted,
            &settlement_session,
            &settlement_operation,
            &cancel,
            &settlement_connection_cancel,
        )
        .await;
        let _cancel_cause = cancel_registration.finish();
        let _ = result_tx.send(result);
    });

    match result_rx.await {
        Ok(result) => result,
        Err(_response_future_dropped) => Err(rpc_err(
            SESSION_BUSY,
            "Context restore was detached from this connection before completion; it settles \
             in the background and the session stays reserved until then",
        )),
    }
}

async fn run_restore(
    ctx: &Arc<super::context::RpcContext>,
    rpc: &Arc<RpcOutbound>,
    store: &Arc<AcpSessionStore>,
    admitted: &AdmittedSession,
    session_id: &str,
    operation_id: &str,
    cancel: &CancellationToken,
    connection_cancel: &CancellationToken,
) -> Result<SessionRestoreContextResult, JsonRpcError> {
    let expected_history = admitted.agent.lock().await.history().to_vec();

    let snapshot = read_snapshot(store, session_id, operation_id).await?;
    let session_row_id = snapshot.session_row_id;

    if cancel.is_cancelled() || connection_cancel.is_cancelled() {
        return Err(rpc_err(
            SESSION_BUSY,
            "Context restore was cancelled; nothing was changed",
        ));
    }

    // Durable deactivation first (joined, never detached, inside the
    // settlement task): it fences on the exact checkpoint identity this
    // request snapshotted, so a stale restore cannot deactivate a later
    // operation's checkpoint, and after it commits the projected reader
    // returns originals for this session.
    let expected_active = snapshot.active_checkpoint.as_ref().map(|checkpoint| {
        (
            checkpoint.operation_id.clone(),
            checkpoint.covered_through_message_id,
        )
    });
    let commit_session = session_id.to_string();
    let commit_operation = operation_id.to_string();
    let store_for_commit = Arc::clone(store);
    let deactivation = tokio::task::spawn_blocking(move || {
        store_for_commit.deactivate_compaction_checkpoint(
            &commit_session,
            session_row_id,
            &commit_operation,
            expected_active
                .as_ref()
                .map(|(operation, covered)| (operation.as_str(), *covered)),
        )
    })
    .await
    .map_err(|join| {
        rpc_err(
            INTERNAL_ERROR,
            format!("Restore commit task failed: {join}"),
        )
    })?
    .map_err(deactivation_failure)?;
    let (status, covered_turns, covered_message_rows) = match deactivation {
        CompactionDeactivationOutcome::Deactivated {
            covered_through_message_id,
            covered_message_rows,
        } => {
            let covered_turns = snapshot
                .terminal_ranges
                .iter()
                .filter(|range| {
                    range.last_message_id <= covered_through_message_id
                        && range.kind == TerminalRangeKind::Completed
                })
                .count();
            (
                "deactivated",
                Some(covered_turns),
                Some(covered_message_rows),
            )
        }
        CompactionDeactivationOutcome::AlreadyDeactivated => ("already_deactivated", None, None),
        CompactionDeactivationOutcome::NoActiveCheckpoint => ("no_active_checkpoint", None, None),
    };

    // Live install: the committed post-restore projection — retained
    // originals plus later turns under the existing visible limits — read
    // from the same durable snapshot, replacing the live projection (or
    // invalidating the exact incarnation on mismatch).
    // A committed restore retry may coexist with a newer active checkpoint.
    // Read the authoritative projection after settlement, never force originals.
    let current = match read_snapshot(store, session_id, operation_id).await {
        Ok(current) if current.session_row_id == session_row_id => current,
        result => {
            invalidate_live_incarnation(ctx, session_id, admitted.generation).await;
            return Err(match result {
                Err(error) => error,
                Ok(_) => rpc_err(SESSION_NOT_FOUND, "Session was replaced during restore"),
            });
        }
    };
    let projection =
        projected_provider_history(&current.message_rows, current.active_checkpoint.as_ref());
    let installed = install_projection(
        ctx,
        rpc,
        session_id,
        &admitted.agent,
        &expected_history,
        projection,
        admitted.generation,
    )
    .await;

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
            .with_category(::zeroclaw_log::EventCategory::Agent)
            .with_outcome(::zeroclaw_log::EventOutcome::Success)
            .with_attrs(::serde_json::json!({
                "session_id": session_id,
                "operation_id": operation_id,
                "status": status,
                "installed": installed,
            })),
        "Manual context restore completed"
    );

    Ok(SessionRestoreContextResult {
        session_id: session_id.to_string(),
        operation_id: operation_id.to_string(),
        status: status.to_string(),
        covered_turns,
        covered_message_rows,
        installed,
    })
}
