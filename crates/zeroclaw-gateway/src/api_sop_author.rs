//! SOP authoring surface for the web node editor.
//!
//! HTTP twin of the daemon's `sops/*` RPC methods, backed by the same
//! `zeroclaw_runtime::sop` authoring core (load/save/delete, graph
//! projection, wire edits, trigger registry). All routes require gateway
//! auth. Draft endpoints (`wire-draft`, `graph-draft`) are pure: they
//! transform the submitted SOP and never touch disk.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use super::AppState;
use super::api::require_auth;
use zeroclaw_runtime::sop::SopGraphExt;

#[derive(Debug, Default, serde::Deserialize)]
pub struct RunsQuery {
    #[serde(default)]
    pub sop: Option<String>,
}

fn sops_dir_and_mode(
    state: &AppState,
) -> (std::path::PathBuf, zeroclaw_runtime::sop::SopExecutionMode) {
    let config = state.config.read();
    let workspace = config.shared_workspace_dir();
    let dir = zeroclaw_runtime::sop::resolve_sops_dir(&workspace, config.sop.sops_dir.as_deref());
    let mode = zeroclaw_runtime::sop::parse_execution_mode(&config.sop.default_execution_mode);
    (dir, mode)
}

fn sop_tool_specs(state: &AppState) -> zeroclaw_runtime::sop::ToolSpecs {
    let config = state.config.read();
    let agent = config.agents.keys().min().cloned().unwrap_or_default();
    zeroclaw_runtime::sop::tool_specs_from_config(&config, &agent)
}

pub async fn handle_sops_list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let (dir, mode) = sops_dir_and_mode(&state);
    let sops = zeroclaw_runtime::sop::load_sops_from_directory(&dir, mode);
    Json(serde_json::json!({ "sops": sops })).into_response()
}

pub async fn handle_sop_trigger_sources(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let registry = {
        let config = state.config.read();
        zeroclaw_runtime::sop::registry_from_config(&config)
    };
    Json(registry).into_response()
}

/// Body for `POST /api/tools/param-options`: resolve selectable values
/// for a domain-typed tool parameter. `args` carries sibling arguments
/// already chosen so cascading domains (e.g. peer targets narrowing on
/// a channel) can filter.
#[derive(serde::Deserialize)]
pub struct ParamOptionsBody {
    pub domain: zeroclaw_api::tool::OptionDomain,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub args: serde_json::Value,
}

pub async fn handle_tools_param_options(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ParamOptionsBody>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read();
    let agent_alias = body
        .agent
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string)
        .or_else(|| config.agents.keys().min().cloned())
        .unwrap_or_default();

    let entries = if body.domain == zeroclaw_api::tool::OptionDomain::ToolNames {
        let security = std::sync::Arc::new(
            zeroclaw_config::policy::SecurityPolicy::for_agent(&config, &agent_alias)
                .unwrap_or_default(),
        );
        let tools = zeroclaw_runtime::tools::default_tools(security);
        let refs: Vec<&dyn zeroclaw_api::tool::Tool> =
            tools.iter().map(std::convert::AsRef::as_ref).collect();
        zeroclaw_runtime::tools::param_options::resolve_options(
            body.domain,
            &config,
            &agent_alias,
            &body.args,
            &refs,
        )
    } else {
        zeroclaw_runtime::tools::param_options::resolve_options(
            body.domain,
            &config,
            &agent_alias,
            &body.args,
            &[],
        )
    };
    Json(serde_json::json!({ "options": entries })).into_response()
}

pub async fn handle_sop_graph(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let (dir, mode) = sops_dir_and_mode(&state);
    match zeroclaw_runtime::sop::load_sop_by_name(&dir, &name, mode) {
        Ok(sop) => {
            let graph =
                zeroclaw_runtime::sop::SopGraph::from_sop_with_specs(&sop, &sop_tool_specs(&state));
            Json(graph).into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("SOP '{name}': {e}") })),
        )
            .into_response(),
    }
}

/// Body for `POST /api/sops/{name}/run`: fire a Manual trigger. `payload` is
/// an optional JSON string handed to the run as the step-1 input. Deserializes
/// from the shared `SopRunRequest` shape (minus `name`, which comes from the
/// path).
#[derive(serde::Deserialize)]
pub struct SopRunBody {
    #[serde(default)]
    pub payload: Option<String>,
}

/// Fire a Manual run for the named SOP and return its `run_id`.
///
/// Thin exposure of the engine dispatch path the `sop_execute` tool uses:
/// builds a Manual `SopEvent` and calls `dispatch_sop_event_to`, which still
/// requires the SOP to declare a matching Manual trigger. The returned
/// `run_id` feeds straight into `sops/{name}/runs/{run_id}/overlay`.
pub async fn handle_sop_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<SopRunBody>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    if let Some(payload) = body.payload.as_deref()
        && !payload.trim().is_empty()
        && serde_json::from_str::<serde_json::Value>(payload).is_err()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "payload is not valid JSON" })),
        )
            .into_response();
    }

    let Some(engine) = state.sop_engine.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "SOP subsystem not enabled" })),
        )
            .into_response();
    };
    let Some(audit) = state.sop_audit.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "SOP subsystem not enabled" })),
        )
            .into_response();
    };

    let payload = body
        .payload
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string);

    let event = zeroclaw_runtime::sop::SopEvent {
        source: zeroclaw_runtime::sop::SopTriggerSource::Manual,
        topic: None,
        payload,
        timestamp: zeroclaw_runtime::sop::engine::now_iso8601(),
    };

    let results =
        zeroclaw_runtime::sop::dispatch::dispatch_sop_event_to(engine, audit, event, &name).await;
    zeroclaw_runtime::sop::dispatch::process_headless_results(&results);

    for result in &results {
        match result {
            zeroclaw_runtime::sop::dispatch::DispatchResult::Started { run_id, action, .. } => {
                let needs_driver = matches!(
                    action.as_ref(),
                    zeroclaw_runtime::sop::SopRunAction::ExecuteStep { .. }
                        | zeroclaw_runtime::sop::SopRunAction::DeterministicStep { .. }
                );
                if needs_driver {
                    let config = state.config.read().clone();
                    zeroclaw_runtime::sop::spawn_headless_run_driver(
                        config,
                        std::sync::Arc::clone(engine),
                        Some(std::sync::Arc::clone(audit)),
                        action.as_ref().clone(),
                    );
                }
                return Json(serde_json::json!({ "run_id": run_id })).into_response();
            }
            zeroclaw_runtime::sop::dispatch::DispatchResult::Skipped { reason, .. } => {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({ "error": reason })),
                )
                    .into_response();
            }
            zeroclaw_runtime::sop::dispatch::DispatchResult::BlockedUnsafe { reason, .. } => {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(serde_json::json!({ "error": reason })),
                )
                    .into_response();
            }
            zeroclaw_runtime::sop::dispatch::DispatchResult::Deferred { reason, .. } => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({ "error": reason })),
                )
                    .into_response();
            }
            zeroclaw_runtime::sop::dispatch::DispatchResult::Coalesced {
                existing_run_id, ..
            } => {
                return Json(serde_json::json!({ "run_id": existing_run_id })).into_response();
            }
            zeroclaw_runtime::sop::dispatch::DispatchResult::NoMatch => {}
        }
    }

    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": format!("SOP '{name}' has no matching manual trigger")
        })),
    )
        .into_response()
}

pub async fn handle_sop_runs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RunsQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let Some(engine) = state.sop_engine.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "SOP subsystem not enabled" })),
        )
            .into_response();
    };
    match zeroclaw_runtime::sop::run_summaries_for(engine, query.sop.as_deref()) {
        Ok(runs) => Json(serde_json::json!({ "runs": runs })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn handle_sop_run_overlay(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((name, run_id)): Path<(String, String)>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let (dir, mode) = sops_dir_and_mode(&state);
    let sop = match zeroclaw_runtime::sop::load_sop_by_name(&dir, &name, mode) {
        Ok(sop) => sop,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": format!("SOP '{name}': {e}") })),
            )
                .into_response();
        }
    };
    let Some(engine) = state.sop_engine.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "SOP subsystem not enabled" })),
        )
            .into_response();
    };
    match zeroclaw_runtime::sop::run_overlay_for(&sop, engine, &run_id) {
        Ok(overlay) => Json(overlay).into_response(),
        Err(e) => {
            let msg = e.to_string();
            let code = if msg.contains("not found") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (code, Json(serde_json::json!({ "error": msg }))).into_response()
        }
    }
}

/// Resolve a gated live run. Body carries the raw `ApprovalDecision` wire
/// value. Waiting approvals and deterministic checkpoints both resolve through
/// the broker-backed chokepoint with an HTTP principal, so named approval
/// policies, membership, and quorum are enforced before a gate or checkpoint can
/// clear. Returns the refreshed overlay.
pub async fn handle_sop_decide(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((name, run_id)): Path<(String, String)>,
    Json(decision_value): Json<serde_json::Value>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    // Derive the transport-authenticated approval subject (the paired-token hash)
    // from the validated bearer, mirroring the /admin/sop approval route's
    // `authorize`, so the broker can enforce a required-group / quorum policy on this
    // authoring surface instead of resolving an anonymous `http(None)` past it. Gated
    // on `require_pairing`: when pairing is OFF every token is a no-op pass-through, so
    // deriving an identity from an unauthenticated header would let any caller fabricate
    // an approval subject - fall back to `http(None)` (which fails a required-group
    // policy closed) in that mode, matching `authorize`.
    let subject = state
        .pairing
        .require_pairing()
        .then(|| crate::api::extract_bearer_token(&headers))
        .flatten()
        .and_then(|t| state.pairing.authenticate_and_hash(t));
    let principal = zeroclaw_runtime::sop::approval::ApprovalPrincipal::http(subject);
    let decision: zeroclaw_runtime::sop::approval::ApprovalDecision =
        match serde_json::from_value(decision_value) {
            Ok(d) => d,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": format!("decision is not a valid approval decision: {e}")
                    })),
                )
                    .into_response();
            }
        };
    let (dir, mode) = sops_dir_and_mode(&state);
    let sop = match zeroclaw_runtime::sop::load_sop_by_name(&dir, &name, mode) {
        Ok(sop) => sop,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": format!("SOP '{name}': {e}") })),
            )
                .into_response();
        }
    };
    let Some(engine) = state.sop_engine.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "SOP subsystem not enabled" })),
        )
            .into_response();
    };

    let agent_alias = sop.agent.clone().unwrap_or_default();
    let span = ::zeroclaw_log::info_span!(
        target: "zeroclaw_log_internal_scope",
        "zeroclaw_scope",
        session_key = %run_id,
        agent_alias = %agent_alias,
        channel = "gateway",
    );
    let _guard = span.enter();

    let mut resolved_outcome = None;
    let mut pending_quorum = false;
    {
        let mut guard = match engine.lock() {
            Ok(g) => g,
            Err(_) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "SOP engine lock poisoned" })),
                )
                    .into_response();
            }
        };
        let run_sop_name = match guard.get_run(&run_id).map(|run| run.sop_name.clone()) {
            Some(name) => name,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({
                        "error": format!("Run {run_id} not found")
                    })),
                )
                    .into_response();
            }
        };
        if run_sop_name != name {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("run '{run_id}' belongs to SOP '{run_sop_name}', not '{name}'")
                })),
            )
                .into_response();
        }
        let status = guard.get_run(&run_id).map(|r| r.status);
        match status {
            Some(
                zeroclaw_runtime::sop::types::SopRunStatus::WaitingApproval
                | zeroclaw_runtime::sop::types::SopRunStatus::PausedCheckpoint,
            ) => {
                use zeroclaw_runtime::sop::approval::{BrokerOutcome, ResolveOutcome};
                // Route through the broker (membership + quorum), not `resolve_gate`
                // directly, otherwise this authoring surface would
                // clear a policied approval gate without enforcing group membership or
                // quorum. With no `[sop.approval]` policy this is exactly `resolve_gate`.
                match guard.resolve_via_broker(&run_id, decision, principal) {
                    Ok(outcome @ BrokerOutcome::Resolved(ResolveOutcome::Resumed(_))) => {
                        resolved_outcome = Some(outcome);
                    }
                    Ok(BrokerOutcome::Resolved(
                        ResolveOutcome::Denied
                        | ResolveOutcome::AlreadyResolved
                        | ResolveOutcome::Revised,
                    )) => {}
                    Ok(
                        BrokerOutcome::Resolved(ResolveOutcome::NotWaiting)
                        | BrokerOutcome::NotWaiting,
                    ) => {
                        return (
                            StatusCode::CONFLICT,
                            Json(serde_json::json!({
                                "error": format!("Run {run_id} is not waiting for approval")
                            })),
                        )
                            .into_response();
                    }
                    Ok(BrokerOutcome::Resolved(ResolveOutcome::RejectedSelfApproval)) => {
                        return (
                            StatusCode::FORBIDDEN,
                            Json(serde_json::json!({
                                "error": "approval_mode forbids this principal from clearing the gate"
                            })),
                        )
                            .into_response();
                    }
                    Ok(BrokerOutcome::Resolved(ResolveOutcome::DeferredAtCapacity)) => {
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(serde_json::json!({
                                "outcome": "deferred_at_capacity",
                                "run_id": run_id,
                            })),
                        )
                            .into_response();
                    }
                    Ok(BrokerOutcome::NotAuthorized { required_group }) => {
                        return (
                            StatusCode::FORBIDDEN,
                            Json(serde_json::json!({
                                "error": format!("not authorized: requires group '{required_group}'")
                            })),
                        )
                            .into_response();
                    }
                    // A step naming an absent policy is a server-side config defect:
                    // fail closed (gate left waiting), never a silent clear.
                    Ok(BrokerOutcome::PolicyMissing { name }) => {
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({
                                "error": format!("approval policy '{name}' is not configured (gate left waiting)")
                            })),
                        )
                            .into_response();
                    }
                    Ok(BrokerOutcome::PolicyUnavailable { reason }) => {
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({
                                "error": "policy_unavailable",
                                "reason": reason,
                            })),
                        )
                            .into_response();
                    }
                    // The vote counted but quorum is not yet met: the gate stays
                    // waiting for the remaining approvers.
                    Ok(BrokerOutcome::PendingQuorum { .. }) => {
                        pending_quorum = true;
                    }
                    Err(e) => {
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({ "error": e.to_string() })),
                        )
                            .into_response();
                    }
                }
            }
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": format!(
                            "Run {run_id} is not waiting for approval or paused at a checkpoint"
                        )
                    })),
                )
                    .into_response();
            }
        }
    }

    if pending_quorum {
        return match zeroclaw_runtime::sop::run_overlay_for(&sop, engine, &run_id) {
            Ok(overlay) => (StatusCode::ACCEPTED, Json(overlay)).into_response(),
            Err(e) => {
                let msg = e.to_string();
                let code = if msg.contains("not found") {
                    StatusCode::NOT_FOUND
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                };
                (code, Json(serde_json::json!({ "error": msg }))).into_response()
            }
        };
    }

    if let Some(outcome) = resolved_outcome {
        let config = state.config.read();
        zeroclaw_runtime::sop::drive_resumed_broker_action(
            &config,
            std::sync::Arc::clone(engine),
            state.sop_audit.clone(),
            &outcome,
        );
    }

    match zeroclaw_runtime::sop::run_overlay_for(&sop, engine, &run_id) {
        Ok(overlay) => Json(overlay).into_response(),
        Err(e) => {
            let msg = e.to_string();
            let code = if msg.contains("not found") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (code, Json(serde_json::json!({ "error": msg }))).into_response()
        }
    }
}

pub async fn handle_sop_full(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let (dir, mode) = sops_dir_and_mode(&state);
    match zeroclaw_runtime::sop::load_sop_by_name(&dir, &name, mode) {
        Ok(sop) => Json(sop).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("SOP '{name}': {e}") })),
        )
            .into_response(),
    }
}

pub async fn handle_sop_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(sop): Json<zeroclaw_runtime::sop::Sop>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let (dir, _mode) = sops_dir_and_mode(&state);
    match zeroclaw_runtime::sop::create_sop_typed(&dir, &sop) {
        Ok(()) => Json(serde_json::json!({ "created": sop.name })).into_response(),
        Err(e) => {
            let code = match e {
                zeroclaw_runtime::sop::SopAuthorError::AlreadyExists(_) => StatusCode::CONFLICT,
                _ => StatusCode::BAD_REQUEST,
            };
            (code, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
        }
    }
}

pub async fn handle_sop_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(mut sop): Json<zeroclaw_runtime::sop::Sop>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    if !sop.name.is_empty() && sop.name != name {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!(
                    "body name '{}' does not match URL name '{name}'",
                    sop.name
                )
            })),
        )
            .into_response();
    }
    sop.name = name;
    let (dir, _mode) = sops_dir_and_mode(&state);
    match zeroclaw_runtime::sop::save_sop(&dir, &sop) {
        Ok(()) => Json(serde_json::json!({ "saved": sop.name })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn handle_sop_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let (dir, _mode) = sops_dir_and_mode(&state);
    match zeroclaw_runtime::sop::delete_sop_typed(&dir, &name) {
        Ok(()) => Json(serde_json::json!({ "deleted": name })).into_response(),
        Err(e) => {
            let code = match e {
                zeroclaw_runtime::sop::SopAuthorError::NotFound(_) => StatusCode::NOT_FOUND,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (code, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
        }
    }
}

/// Body for `wire-draft`: a full draft SOP plus one edit to apply.
#[derive(serde::Deserialize)]
pub struct WireDraftRequest {
    pub sop: zeroclaw_runtime::sop::Sop,
    pub edit: zeroclaw_runtime::sop::WireEdit,
}

pub async fn handle_sop_wire_draft(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<WireDraftRequest>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let mut sop = req.sop;
    if let Err(e) = zeroclaw_runtime::sop::apply_wire(&mut sop, &req.edit) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response();
    }
    let graph = zeroclaw_runtime::sop::SopGraph::from_sop_with_specs(&sop, &sop_tool_specs(&state));
    Json(serde_json::json!({ "sop": sop, "graph": graph })).into_response()
}

/// Body for `graph-draft`: a full draft SOP to project without saving.
#[derive(serde::Deserialize)]
pub struct GraphDraftRequest {
    pub sop: zeroclaw_runtime::sop::Sop,
}

pub async fn handle_sop_graph_draft(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<GraphDraftRequest>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let graph =
        zeroclaw_runtime::sop::SopGraph::from_sop_with_specs(&req.sop, &sop_tool_specs(&state));
    Json(graph).into_response()
}

pub async fn handle_sop_graph_legend(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    Json(zeroclaw_runtime::sop::GraphLegend::canonical()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::extract::{Path, State};
    use axum::http::{HeaderValue, header};
    use http_body_util::BodyExt;
    use zeroclaw_config::schema::{
        ApprovalGroupConfig, ApprovalPolicyConfig, SopApprovalConfig, SopConfig,
    };
    use zeroclaw_runtime::security::pairing::PairingGuard;
    use zeroclaw_runtime::sop::approval::ApprovalBroker;
    use zeroclaw_runtime::sop::engine::{SopEngine, now_iso8601};
    use zeroclaw_runtime::sop::types::{
        Sop, SopAdmissionPolicy, SopEvent, SopExecutionMode, SopPriority, SopRunAction,
        SopRunStatus, SopStep, SopStepKind, SopTrigger, SopTriggerSource,
    };

    fn bearer(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        headers
    }

    fn authoring_policy_sop() -> Sop {
        Sop {
            name: "deploy".into(),
            description: "t".into(),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Supervised,
            triggers: vec![SopTrigger::Manual],
            steps: vec![SopStep {
                number: 1,
                title: "gate".into(),
                requires_confirmation: true,
                kind: SopStepKind::Execute,
                policy: Some("prod".into()),
                ..SopStep::default()
            }],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
            agent: None,
            admission_policy: SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
        }
    }

    fn authoring_checkpoint_sop(name: &str) -> Sop {
        Sop {
            name: name.into(),
            description: format!("{name} checkpoint"),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Deterministic,
            triggers: vec![SopTrigger::Manual],
            steps: vec![SopStep {
                number: 1,
                title: "gate".into(),
                kind: SopStepKind::Checkpoint,
                ..SopStep::default()
            }],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: true,
            agent: None,
            admission_policy: SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
        }
    }

    fn authoring_state_with_policied_gate(
        member_token: &str,
        other_token: &str,
    ) -> (tempfile::TempDir, AppState, String) {
        let tmp = tempfile::tempdir().unwrap();
        let sops_dir = tmp.path().join("sops");
        let member_hash = PairingGuard::token_hash(member_token);

        let mut groups = HashMap::new();
        groups.insert(
            "release".to_string(),
            ApprovalGroupConfig {
                members: vec![member_hash],
            },
        );
        let mut policies = HashMap::new();
        policies.insert(
            "prod".to_string(),
            ApprovalPolicyConfig {
                required_group: Some("release".into()),
                quorum: 1,
                request_route: None,
                escalation_route: None,
            },
        );
        let approval = SopApprovalConfig { groups, policies };
        let sop = authoring_policy_sop();
        zeroclaw_runtime::sop::save_sop(&sops_dir, &sop).unwrap();

        let mut engine = SopEngine::new(SopConfig {
            approval,
            ..SopConfig::default()
        })
        .with_approval_broker(Arc::new(ApprovalBroker::disabled()));
        engine.set_sops_for_test(vec![sop]);
        let action = engine
            .start_run(
                "deploy",
                SopEvent {
                    source: SopTriggerSource::Manual,
                    topic: None,
                    payload: None,
                    timestamp: now_iso8601(),
                },
            )
            .unwrap();
        let run_id = match action {
            SopRunAction::WaitApproval { run_id, .. } => run_id,
            other => panic!("expected WaitApproval, got {other:?}"),
        };

        let mut config = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        config.sop.sops_dir = Some(sops_dir.to_string_lossy().into_owned());
        let mut state = crate::api::test_state(config);
        state.sop_engine = Some(Arc::new(Mutex::new(engine)));
        state.pairing = Arc::new(PairingGuard::new(
            true,
            &[member_token.to_string(), other_token.to_string()],
        ));
        (tmp, state, run_id)
    }

    #[tokio::test]
    async fn authoring_decide_enforces_broker_policy_membership() {
        let member = "member-token";
        let outsider = "outsider-token";
        let (_tmp, state, run_id) = authoring_state_with_policied_gate(member, outsider);

        let resp = handle_sop_decide(
            State(state.clone()),
            bearer(outsider),
            Path(("deploy".to_string(), run_id.clone())),
            Json(serde_json::json!("approve")),
        )
        .await;

        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "a paired non-member must not clear a policied authoring gate"
        );
        let status = state
            .sop_engine
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .get_run(&run_id)
            .map(|r| r.status);
        assert_eq!(
            status,
            Some(SopRunStatus::WaitingApproval),
            "the gate stays waiting after a broker-rejected authoring decision"
        );
    }

    #[tokio::test]
    async fn authoring_decide_pending_quorum_returns_overlay_shape() {
        let first_member = "member-token-1";
        let second_member = "member-token-2";
        let tmp = tempfile::tempdir().unwrap();
        let sops_dir = tmp.path().join("sops");
        let first_hash = PairingGuard::token_hash(first_member);
        let second_hash = PairingGuard::token_hash(second_member);

        let mut groups = HashMap::new();
        groups.insert(
            "release".to_string(),
            ApprovalGroupConfig {
                members: vec![first_hash, second_hash],
            },
        );
        let mut policies = HashMap::new();
        policies.insert(
            "prod".to_string(),
            ApprovalPolicyConfig {
                required_group: Some("release".into()),
                quorum: 2,
                request_route: None,
                escalation_route: None,
            },
        );
        let approval = SopApprovalConfig { groups, policies };
        let sop = authoring_policy_sop();
        zeroclaw_runtime::sop::save_sop(&sops_dir, &sop).unwrap();

        let mut engine = SopEngine::new(SopConfig {
            approval,
            ..SopConfig::default()
        })
        .with_approval_broker(Arc::new(ApprovalBroker::disabled()));
        engine.set_sops_for_test(vec![sop]);
        let action = engine
            .start_run(
                "deploy",
                SopEvent {
                    source: SopTriggerSource::Manual,
                    topic: None,
                    payload: None,
                    timestamp: now_iso8601(),
                },
            )
            .unwrap();
        let run_id = match action {
            SopRunAction::WaitApproval { run_id, .. } => run_id,
            other => panic!("expected WaitApproval, got {other:?}"),
        };

        let mut config = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        config.sop.sops_dir = Some(sops_dir.to_string_lossy().into_owned());
        let mut state = crate::api::test_state(config);
        state.sop_engine = Some(Arc::new(Mutex::new(engine)));
        state.pairing = Arc::new(PairingGuard::new(
            true,
            &[first_member.to_string(), second_member.to_string()],
        ));

        let resp = handle_sop_decide(
            State(state.clone()),
            bearer(first_member),
            Path(("deploy".to_string(), run_id.clone())),
            Json(serde_json::json!("approve")),
        )
        .await;

        assert_eq!(
            resp.status(),
            StatusCode::ACCEPTED,
            "the first quorum member should leave the run pending"
        );
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json.get("run_id").and_then(|value| value.as_str()),
            Some(run_id.as_str())
        );
        assert_eq!(
            json.get("sop_name").and_then(|value| value.as_str()),
            Some("deploy")
        );
        assert_eq!(
            json.get("waiting").and_then(|value| value.as_bool()),
            Some(true)
        );
        assert!(
            json.get("outcome").is_none(),
            "pending quorum responses must keep the RunOverlay shape, got {json:?}"
        );
    }

    #[tokio::test]
    async fn authoring_decide_rejects_run_id_from_different_sop_before_broker_resolution() {
        let token = "member-token";
        let tmp = tempfile::tempdir().unwrap();
        let sops_dir = tmp.path().join("sops");
        let sop_a = authoring_checkpoint_sop("deploy-a");
        let sop_b = authoring_checkpoint_sop("deploy-b");
        zeroclaw_runtime::sop::save_sop(&sops_dir, &sop_a).unwrap();
        zeroclaw_runtime::sop::save_sop(&sops_dir, &sop_b).unwrap();

        let mut engine = SopEngine::new(SopConfig::default());
        engine.set_sops_for_test(vec![sop_a, sop_b]);
        let action = engine
            .start_run(
                "deploy-b",
                SopEvent {
                    source: SopTriggerSource::Manual,
                    topic: None,
                    payload: None,
                    timestamp: now_iso8601(),
                },
            )
            .unwrap();
        let run_id = match action {
            SopRunAction::CheckpointWait { run_id, .. } => run_id,
            other => panic!("expected CheckpointWait, got {other:?}"),
        };

        let mut config = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        config.sop.sops_dir = Some(sops_dir.to_string_lossy().into_owned());
        let mut state = crate::api::test_state(config);
        state.sop_engine = Some(Arc::new(Mutex::new(engine)));
        state.pairing = Arc::new(PairingGuard::new(true, &[token.to_string()]));

        let resp = handle_sop_decide(
            State(state.clone()),
            bearer(token),
            Path(("deploy-a".to_string(), run_id.clone())),
            Json(serde_json::json!("approve")),
        )
        .await;

        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "a path SOP must not resolve a run owned by another SOP"
        );
        let guard = state.sop_engine.as_ref().unwrap().lock().unwrap();
        let run = guard.get_run(&run_id).expect("deploy-b run remains active");
        assert_eq!(run.sop_name, "deploy-b");
        assert_eq!(run.status, SopRunStatus::PausedCheckpoint);
        assert!(
            !guard
                .run_events(&run_id)
                .unwrap_or_default()
                .iter()
                .any(|event| event.kind == "gate_resolved"),
            "mismatched authoring decision must not append a gate_resolved row"
        );
    }
}
