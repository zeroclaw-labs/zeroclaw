//! Manual, recoverable context compaction for native ZeroCode Code sessions.
//!
//! The durable ACP row remains canonical and summary-free. A single derived
//! checkpoint records the summary projection and the exact canonical prefix
//! it replaces. The live Agent receives that projection only after the
//! checkpoint commits; a failed reconciliation removes only the admitted
//! live generation so the next prompt rehydrates from durable state.

use super::dispatch::rpc_err;
use super::session::IdleOperationAdmission;
use super::types::*;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use zeroclaw_api::agent::RetainedContextSnapshot;
use zeroclaw_api::jsonrpc::JsonRpcError;
use zeroclaw_api::jsonrpc::error_codes::*;
use zeroclaw_api::model_provider::{ChatMessage, ConversationMessage};
use zeroclaw_infra::acp_session_store::{
    AcpActiveCheckpointRecord, AcpCompactionSnapshot, AcpSessionStore, CompactionActivationError,
    CompactionActivationOutcome, CompactionActivationRequest, CompactionDeactivationError,
    CompactionDeactivationOutcome, CompactionSourceError, select_compaction_source,
};

pub(crate) const COMPACTION_FORMAT_VERSION: i64 = 1;

const OPERATION_DEADLINE: std::time::Duration = std::time::Duration::from_secs(180);
const MAX_SUMMARY_CHARS: usize = 12_000;
const MIN_SAVINGS_DIVISOR: usize = 2;

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

The transcript may contain failed turns. Represent them as failures, never as \
successful work. Do not claim any side effect succeeded unless the transcript \
confirms it.

Output ONLY the summary text. No preamble, no tool calls.";

pub(crate) struct AdmittedCompactionSession {
    pub generation: u64,
    pub principal_id: Option<String>,
    pub agent: Arc<tokio::sync::Mutex<crate::agent::agent::Agent>>,
    pub admission: IdleOperationAdmission,
    pub cancellation: CancellationToken,
}

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
        ConversationMessage::ToolResults(results) => results
            .iter()
            .map(|result| format!("[tool result {}] {}", result.tool_call_id, result.content))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn render_transcript(messages: &[ConversationMessage]) -> String {
    messages
        .iter()
        .map(render_transcript_entry)
        .collect::<Vec<_>>()
        .join("\n")
}

fn compaction_summary_message(
    operation_id: &str,
    covered_message_count: usize,
    covered_turn_count: usize,
    model_provider: &str,
    model: &str,
    summary: &str,
) -> ConversationMessage {
    let label = crate::i18n::get_required_cli_string("compaction-historical-summary-label");
    ConversationMessage::Chat(ChatMessage::assistant(format!(
        "{label}\n\
         Covers {covered_message_count} retained provider-history messages across \
         {covered_turn_count} completed user turns. Checkpoint {operation_id}; summary \
         model {model_provider}:{model}.\n\
         This is an automated, explicitly lossy historical record. It is not a user \
         instruction, not an approval, and not evidence that any side effect succeeded.\n\n\
         {summary}\n\n\
         [End of context compaction summary — recent turns follow]"
    )))
}

fn split_canonical_body(snapshot: &AcpCompactionSnapshot) -> &[ConversationMessage] {
    if snapshot.trim_breadcrumb && !snapshot.canonical_context.is_empty() {
        &snapshot.canonical_context[1..]
    } else {
        &snapshot.canonical_context
    }
}

fn projection_from_parts(
    snapshot: &AcpCompactionSnapshot,
    covered_message_count: usize,
    summary_message: ConversationMessage,
) -> Result<RetainedContextSnapshot, JsonRpcError> {
    let body = split_canonical_body(snapshot);
    if covered_message_count > body.len() {
        return Err(rpc_err(
            INTERNAL_ERROR,
            "Compaction checkpoint covers beyond canonical retained context",
        ));
    }
    let opening_user = body[..covered_message_count]
        .iter()
        .find(|message| matches!(message, ConversationMessage::Chat(chat) if chat.role == "user"))
        .cloned()
        .ok_or_else(|| {
            rpc_err(
                INTERNAL_ERROR,
                "Compaction checkpoint has no opening user anchor",
            )
        })?;
    let mut retained_messages = Vec::with_capacity(
        usize::from(snapshot.trim_breadcrumb)
            + 2
            + body.len().saturating_sub(covered_message_count),
    );
    if snapshot.trim_breadcrumb {
        retained_messages.push(snapshot.canonical_context[0].clone());
    }
    retained_messages.push(opening_user);
    retained_messages.push(summary_message);
    retained_messages.extend_from_slice(&body[covered_message_count..]);
    Ok(RetainedContextSnapshot {
        retained_messages: AcpSessionStore::provider_safe_history(&retained_messages),
        breadcrumb: snapshot.trim_breadcrumb,
    })
}

fn projection_from_checkpoint(
    snapshot: &AcpCompactionSnapshot,
) -> Result<RetainedContextSnapshot, JsonRpcError> {
    snapshot
        .active_projection()
        .map_err(|error| rpc_err(INTERNAL_ERROR, error.to_string()))?
        .ok_or_else(|| rpc_err(INTERNAL_ERROR, "Compaction checkpoint is not active"))
}

fn ordinary_projection(snapshot: &AcpCompactionSnapshot) -> RetainedContextSnapshot {
    RetainedContextSnapshot {
        retained_messages: AcpSessionStore::provider_safe_history(&snapshot.canonical_context),
        breadcrumb: snapshot.trim_breadcrumb,
    }
}

async fn read_snapshot(
    store: &Arc<AcpSessionStore>,
    session_id: &str,
    principal_id: Option<String>,
) -> Result<AcpCompactionSnapshot, JsonRpcError> {
    let store = Arc::clone(store);
    let session_id = session_id.to_string();
    let snapshot = tokio::task::spawn_blocking(move || {
        store.read_compaction_snapshot_for_owner(&session_id, principal_id.as_deref())
    })
    .await
    .map_err(|error| {
        rpc_err(
            INTERNAL_ERROR,
            format!("Compaction snapshot task failed: {error}"),
        )
    })?
    .map_err(|error| {
        rpc_err(
            INTERNAL_ERROR,
            format!("Failed to read compaction snapshot: {error}"),
        )
    })?
    .ok_or_else(|| rpc_err(SESSION_NOT_FOUND, "Session not found"))?;
    if snapshot.killed {
        return Err(rpc_err(SESSION_NOT_FOUND, "Session is killed"));
    }
    if snapshot.interaction_surface.as_deref()
        != Some(crate::agent::prompt::InteractionSurface::ZerocodeCode.as_str())
    {
        return Err(rpc_err(
            INVALID_PARAMS,
            "Context compaction is only available for native ZeroCode Code sessions",
        ));
    }
    if snapshot.inflight_turn_id.is_some() {
        return Err(rpc_err(
            SESSION_BUSY,
            "A durable turn is still in flight; reopen or recover the session before changing its context",
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
            INVALID_PARAMS,
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
        CompactionActivationError::SourceMismatch => rpc_err(
            INVALID_PARAMS,
            "The retained context changed during compaction; nothing was committed",
        ),
        CompactionActivationError::StaleActiveCheckpoint { .. } => rpc_err(
            INVALID_PARAMS,
            "The active compaction changed during this request; nothing was committed",
        ),
        CompactionActivationError::Storage(detail) => rpc_err(
            INTERNAL_ERROR,
            format!("Compaction commit failed unchanged: {detail}"),
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
            "The active compaction changed during restore; retry the operation",
        ),
        CompactionDeactivationError::Storage(detail) => rpc_err(
            INTERNAL_ERROR,
            format!("Restore failed unchanged: {detail}"),
        ),
    }
}

async fn install_projection(
    ctx: &Arc<super::context::RpcContext>,
    admitted: &AdmittedCompactionSession,
    session_id: &str,
    expected_history: &[ConversationMessage],
    projection: RetainedContextSnapshot,
) -> bool {
    let install = admitted
        .agent
        .lock()
        .await
        .install_conversation_history_projection(expected_history, projection);
    match install {
        crate::agent::agent::HistoryProjectionInstall::Installed => true,
        crate::agent::agent::HistoryProjectionInstall::LiveHistoryMismatch
        | crate::agent::agent::HistoryProjectionInstall::ProjectionDoesNotFit
        | crate::agent::agent::HistoryProjectionInstall::SystemPromptFailed => {
            ctx.sessions
                .remove_generation(session_id, admitted.generation)
                .await;
            false
        }
    }
}

fn result_usage(checkpoint: &AcpActiveCheckpointRecord) -> Option<CompactionUsage> {
    (checkpoint.input_tokens.is_some() || checkpoint.output_tokens.is_some()).then_some(
        CompactionUsage {
            input_tokens: checkpoint.input_tokens,
            output_tokens: checkpoint.output_tokens,
        },
    )
}

async fn compact_result(
    session_id: &str,
    status: &str,
    snapshot: &AcpCompactionSnapshot,
    checkpoint: &AcpActiveCheckpointRecord,
    agent: &Arc<tokio::sync::Mutex<crate::agent::agent::Agent>>,
    before_messages: &[ConversationMessage],
    estimates: Option<(u64, u64)>,
    installed: bool,
) -> Result<SessionCompactContextResult, JsonRpcError> {
    let (estimated_tokens_before, estimated_tokens_after) = match estimates {
        Some(estimates) => estimates,
        None => {
            let projection = projection_from_checkpoint(snapshot)?;
            let agent = agent.lock().await;
            (
                agent.estimate_projection_tokens(before_messages) as u64,
                agent.estimate_projection_tokens(&projection.retained_messages) as u64,
            )
        }
    };
    Ok(SessionCompactContextResult {
        session_id: session_id.to_string(),
        operation_id: checkpoint.operation_id.clone(),
        status: status.to_string(),
        covered_turns: checkpoint.covered_turn_count,
        covered_message_rows: checkpoint.covered_message_count,
        estimated_tokens_before,
        estimated_tokens_after,
        summary: checkpoint.summary.clone(),
        model_provider: checkpoint.summary_model_provider.clone(),
        model: checkpoint.summary_model.clone(),
        usage: result_usage(checkpoint),
        installed,
    })
}

pub(crate) async fn compact_context(
    ctx: Arc<super::context::RpcContext>,
    admitted: AdmittedCompactionSession,
    connection_cancel: CancellationToken,
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
    let (sender, receiver) = oneshot::channel();
    zeroclaw_spawn::spawn!(async move {
        let result = run_compact(
            &ctx,
            &store,
            &admitted,
            &session_id,
            &operation_id,
            &connection_cancel,
        )
        .await;
        let _cancel_cause = admitted.admission.finish();
        let _ = sender.send(result);
    });
    receiver.await.unwrap_or_else(|_| {
        Err(rpc_err(
            SESSION_BUSY,
            "Context compaction continues settling after this connection detached",
        ))
    })
}

async fn run_compact(
    ctx: &Arc<super::context::RpcContext>,
    store: &Arc<AcpSessionStore>,
    admitted: &AdmittedCompactionSession,
    session_id: &str,
    operation_id: &str,
    connection_cancel: &CancellationToken,
) -> Result<SessionCompactContextResult, JsonRpcError> {
    let expected_history = admitted.agent.lock().await.history().to_vec();
    let snapshot = read_snapshot(store, session_id, admitted.principal_id.clone()).await?;
    if let Some(checkpoint) = snapshot.active_checkpoint.as_ref()
        && checkpoint.operation_id == operation_id
    {
        let projection = projection_from_checkpoint(&snapshot)?;
        let installed =
            install_projection(ctx, admitted, session_id, &expected_history, projection).await;
        return compact_result(
            session_id,
            "already_committed",
            &snapshot,
            checkpoint,
            &admitted.agent,
            &expected_history,
            None,
            installed,
        )
        .await;
    }

    let body = split_canonical_body(&snapshot);
    let selection = select_compaction_source(body).map_err(source_refusal)?;
    let covered = &body[..selection.covered_message_count];
    let prompt = format!(
        "{SUMMARIZATION_INSTRUCTIONS}\n\n{}",
        render_transcript(covered)
    );

    let (prompt_tokens, reserve, budget) = {
        let agent = admitted.agent.lock().await;
        let (_, provider, model) = agent.attribution_fields();
        let prompt_message = [ConversationMessage::Chat(ChatMessage::user(prompt.clone()))];
        let placeholder = compaction_summary_message(
            operation_id,
            selection.covered_message_count,
            selection.covered_turn_count,
            &provider,
            &model,
            &"x".repeat(MAX_SUMMARY_CHARS),
        );
        let limits = agent.context_limits();
        (
            agent.estimate_projection_tokens(&prompt_message),
            agent.estimate_projection_tokens(&[placeholder]),
            if limits.context_token_budget == 0 {
                limits.model_context_window
            } else {
                limits.context_token_budget
            },
        )
    };
    if prompt_tokens.saturating_add(reserve) > budget {
        return Err(rpc_err(
            INVALID_PARAMS,
            format!(
                "Cannot compact context: the selected source (~{prompt_tokens} estimated tokens) does not fit the current route budget ({budget})"
            ),
        ));
    }

    let summarization = {
        let agent = admitted.agent.lock().await;
        let call = agent.run_bounded_summarization(
            &prompt,
            MAX_SUMMARY_CHARS,
            OPERATION_DEADLINE,
            &admitted.cancellation,
        );
        tokio::select! {
            biased;
            _ = connection_cancel.cancelled() => {
                admitted.cancellation.cancel();
                Err(crate::agent::agent::BoundedSummarizationError::Cancelled)
            }
            result = call => result,
        }
    }
    .map_err(summarization_failure)?;

    let summary_message = compaction_summary_message(
        operation_id,
        selection.covered_message_count,
        selection.covered_turn_count,
        &summarization.model_provider,
        &summarization.model,
        &summarization.summary,
    );
    let candidate = projection_from_parts(
        &snapshot,
        selection.covered_message_count,
        summary_message.clone(),
    )?;
    let (before_estimate, after_estimate, fits) = {
        let agent = admitted.agent.lock().await;
        (
            agent.estimate_projection_tokens(&expected_history),
            agent.estimate_projection_tokens(&candidate.retained_messages),
            agent.conversation_history_projection_fits(
                &candidate.retained_messages,
                candidate.breadcrumb,
            ),
        )
    };
    if !fits {
        return Err(rpc_err(
            INVALID_PARAMS,
            "Cannot compact context: the summary and retained tail exceed the live message limit; nothing was changed",
        ));
    }
    if after_estimate.saturating_mul(MIN_SAVINGS_DIVISOR) >= before_estimate {
        return Err(rpc_err(
            INVALID_PARAMS,
            "Context compaction refused because the derived projection would not reduce context usefully; nothing was changed",
        ));
    }
    if admitted.cancellation.is_cancelled() || connection_cancel.is_cancelled() {
        return Err(rpc_err(
            SESSION_BUSY,
            "Context compaction was cancelled before commit; the prior projection is unchanged",
        ));
    }

    let commit_store = Arc::clone(store);
    let commit_session = session_id.to_string();
    let commit_principal = admitted.principal_id.clone();
    let commit_operation = operation_id.to_string();
    let commit_source = snapshot.canonical_context_sha256.clone();
    let expected_active = snapshot
        .active_checkpoint
        .as_ref()
        .map(|checkpoint| checkpoint.operation_id.clone());
    let commit_summary = summarization.summary.clone();
    let commit_summary_message = summary_message.clone();
    let commit_provider = summarization.model_provider.clone();
    let commit_model = summarization.model.clone();
    let input_tokens = summarization
        .usage
        .as_ref()
        .and_then(|usage| usage.input_tokens);
    let output_tokens = summarization
        .usage
        .as_ref()
        .and_then(|usage| usage.output_tokens);
    let session_row_id = snapshot.session_row_id;
    let activation = tokio::task::spawn_blocking(move || {
        commit_store.activate_compaction_checkpoint_for_owner(&CompactionActivationRequest {
            session_uuid: &commit_session,
            principal_id: commit_principal.as_deref(),
            expected_session_row_id: session_row_id,
            expected_source_context_sha256: &commit_source,
            expected_active_operation: expected_active.as_deref(),
            format_version: COMPACTION_FORMAT_VERSION,
            operation_id: &commit_operation,
            covered_message_count: selection.covered_message_count,
            covered_turn_count: selection.covered_turn_count,
            summary: &commit_summary,
            summary_message: &commit_summary_message,
            summary_model_provider: &commit_provider,
            summary_model: &commit_model,
            input_tokens,
            output_tokens,
        })
    })
    .await
    .map_err(|error| {
        rpc_err(
            INTERNAL_ERROR,
            format!("Compaction commit task failed: {error}"),
        )
    })?
    .map_err(activation_failure)?;

    let committed = read_snapshot(store, session_id, admitted.principal_id.clone()).await?;
    let checkpoint = committed.active_checkpoint.as_ref().ok_or_else(|| {
        rpc_err(
            INTERNAL_ERROR,
            "Compaction committed without an active checkpoint",
        )
    })?;
    if checkpoint.operation_id != operation_id {
        ctx.sessions
            .remove_generation(session_id, admitted.generation)
            .await;
        return Err(rpc_err(
            INTERNAL_ERROR,
            "A different compaction became active during reconciliation",
        ));
    }
    let projection = projection_from_checkpoint(&committed)?;
    let installed =
        install_projection(ctx, admitted, session_id, &expected_history, projection).await;
    let status = match activation {
        CompactionActivationOutcome::Activated => "activated",
        CompactionActivationOutcome::AlreadyActive => "already_committed",
    };
    compact_result(
        session_id,
        status,
        &committed,
        checkpoint,
        &admitted.agent,
        &expected_history,
        Some((before_estimate as u64, after_estimate as u64)),
        installed,
    )
    .await
}

pub(crate) async fn restore_context(
    ctx: Arc<super::context::RpcContext>,
    admitted: AdmittedCompactionSession,
    connection_cancel: CancellationToken,
    params: SessionRestoreContextParams,
) -> Result<SessionRestoreContextResult, JsonRpcError> {
    let store = ctx
        .acp_session_store
        .clone()
        .ok_or_else(|| rpc_err(INTERNAL_ERROR, "ACP session store is not available"))?;
    let session_id = params.session_id;
    let request_operation_id = params
        .operation_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let (sender, receiver) = oneshot::channel();
    zeroclaw_spawn::spawn!(async move {
        let result = run_restore(
            &ctx,
            &store,
            &admitted,
            &session_id,
            &request_operation_id,
            &connection_cancel,
        )
        .await;
        let _cancel_cause = admitted.admission.finish();
        let _ = sender.send(result);
    });
    receiver.await.unwrap_or_else(|_| {
        Err(rpc_err(
            SESSION_BUSY,
            "Context restore continues settling after this connection detached",
        ))
    })
}

async fn run_restore(
    ctx: &Arc<super::context::RpcContext>,
    store: &Arc<AcpSessionStore>,
    admitted: &AdmittedCompactionSession,
    session_id: &str,
    request_operation_id: &str,
    connection_cancel: &CancellationToken,
) -> Result<SessionRestoreContextResult, JsonRpcError> {
    let expected_history = admitted.agent.lock().await.history().to_vec();
    let snapshot = read_snapshot(store, session_id, admitted.principal_id.clone()).await?;
    let Some(active) = snapshot.active_checkpoint.as_ref() else {
        return Ok(SessionRestoreContextResult {
            session_id: session_id.to_string(),
            operation_id: request_operation_id.to_string(),
            status: "no_active_checkpoint".to_string(),
            covered_turns: None,
            covered_message_rows: None,
            installed: false,
        });
    };
    if admitted.cancellation.is_cancelled() || connection_cancel.is_cancelled() {
        return Err(rpc_err(
            SESSION_BUSY,
            "Context restore was cancelled; nothing was changed",
        ));
    }
    let active_operation = active.operation_id.clone();
    let covered_turns = active.covered_turn_count;
    let covered_message_rows = active.covered_message_count;
    let commit_store = Arc::clone(store);
    let commit_session = session_id.to_string();
    let commit_principal = admitted.principal_id.clone();
    let session_row_id = snapshot.session_row_id;
    let outcome = tokio::task::spawn_blocking(move || {
        commit_store.deactivate_compaction_checkpoint_for_owner(
            &commit_session,
            commit_principal.as_deref(),
            session_row_id,
            &active_operation,
        )
    })
    .await
    .map_err(|error| {
        rpc_err(
            INTERNAL_ERROR,
            format!("Restore commit task failed: {error}"),
        )
    })?
    .map_err(deactivation_failure)?;

    let current = read_snapshot(store, session_id, admitted.principal_id.clone()).await?;
    let installed = install_projection(
        ctx,
        admitted,
        session_id,
        &expected_history,
        ordinary_projection(&current),
    )
    .await;
    let status = match outcome {
        CompactionDeactivationOutcome::Deactivated => "deactivated",
        CompactionDeactivationOutcome::NoActiveCheckpoint => "no_active_checkpoint",
    };
    Ok(SessionRestoreContextResult {
        session_id: session_id.to_string(),
        operation_id: request_operation_id.to_string(),
        status: status.to_string(),
        covered_turns: Some(covered_turns),
        covered_message_rows: Some(covered_message_rows),
        installed,
    })
}
