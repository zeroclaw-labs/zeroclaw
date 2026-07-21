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
/// value. A `WaitingApproval` run resolves through the audited `resolve_gate`
/// chokepoint with an HTTP principal; a deterministic `PausedCheckpoint` run
/// resolves through `decide_checkpoint`. Returns the refreshed overlay.
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

    let mut resumed_action: Option<zeroclaw_runtime::sop::types::SopRunAction> = None;
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
        let status = guard.get_run(&run_id).map(|r| r.status);
        match status {
            Some(zeroclaw_runtime::sop::types::SopRunStatus::WaitingApproval) => {
                use zeroclaw_runtime::sop::approval::{BrokerOutcome, ResolveOutcome};
                // Route through the broker (membership + quorum), not `resolve_gate`
                // directly, otherwise this authoring surface would
                // clear a policied approval gate without enforcing group membership or
                // quorum. With no `[sop.approval]` policy this is exactly `resolve_gate`.
                match guard.resolve_via_broker(&run_id, decision, principal) {
                    Ok(BrokerOutcome::Resolved(ResolveOutcome::Resumed(action))) => {
                        resumed_action = Some(*action);
                    }
                    Ok(BrokerOutcome::Resolved(
                        ResolveOutcome::Denied | ResolveOutcome::AlreadyResolved,
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
                    // The vote counted but quorum is not yet met: the gate stays
                    // waiting for the remaining approvers.
                    Ok(BrokerOutcome::PendingQuorum { have, need }) => {
                        return (
                            StatusCode::ACCEPTED,
                            Json(serde_json::json!({
                                "outcome": format!("pending_quorum ({have}/{need})"),
                                "run_id": run_id,
                            })),
                        )
                            .into_response();
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
            _ => match guard.decide_checkpoint(&run_id, decision) {
                Ok(action) => {
                    resumed_action = Some(action);
                }
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({ "error": e.to_string() })),
                    )
                        .into_response();
                }
            },
        }
    }

    if let Some(action) = resumed_action {
        let config = state.config.read().clone();
        zeroclaw_runtime::sop::spawn_headless_run_driver(
            config,
            std::sync::Arc::clone(engine),
            state.sop_audit.clone(),
            action,
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
