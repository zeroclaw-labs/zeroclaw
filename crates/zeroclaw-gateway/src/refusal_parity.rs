//! Failure-path parity for the routes served through the core.
//!
//! A route served through the core must refuse exactly as its in-process
//! body does: same status, same content type, same body, for the same state.
//! Each case below builds one state, runs the in-process handler on it, asks
//! the core the same question over the daemon's real in-process connector,
//! and renders that refusal the way both routers render a `*_through_core`
//! error: the daemon's router through `CoreError::into_response`, and
//! `zeroclaw-gw` through `explain`. All three answers must agree.
//!
//! Ports reuse [`assert_refusal_parity`] and [`core_refusal`] for their own
//! routes, and [`oversized`] and [`incomplete`] for the body cases every
//! router must answer alike.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method as HttpMethod, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use zeroclaw_config::schema::SopApprovalConfig;
use zeroclaw_rpc_client::Method;
use zeroclaw_runtime::rpc::context::RpcContext;
use zeroclaw_runtime::rpc::inproc::InprocConnector;
use zeroclaw_runtime::sop::approval::ApprovalBroker;
use zeroclaw_runtime::sop::engine::{SopEngine, now_iso8601};
use zeroclaw_runtime::sop::types::{
    Sop, SopAdmissionPolicy, SopEvent, SopExecutionMode, SopPriority, SopRunAction, SopStep,
    SopStepKind, SopTrigger, SopTriggerSource,
};

use crate::AppState;
use crate::api_sop_author::{
    RunsQuery, SopRunBody, handle_sop_create, handle_sop_full, handle_sop_run, handle_sop_runs,
};
use crate::core_rpc::{CoreAccess, CoreCall, CoreRpc};

// ── The helper ports reuse ───────────────────────────────────────

/// One HTTP answer as a client sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Answer {
    pub(crate) status: StatusCode,
    pub(crate) content_type: Option<String>,
    pub(crate) body: AnswerBody,
}

/// A body compared as JSON when it parses as JSON, byte for byte otherwise
/// (a framework rejection answers plain text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AnswerBody {
    Json(Value),
    Bytes(Vec<u8>),
}

/// Read `response` to the end.
pub(crate) async fn answer(response: Response) -> Answer {
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("a readable body")
        .to_bytes();
    let body = serde_json::from_slice(&bytes)
        .map_or_else(|_| AnswerBody::Bytes(bytes.to_vec()), AnswerBody::Json);
    Answer {
        status,
        content_type,
        body,
    }
}

/// Require one failure case to answer alike on every path.
///
/// `expected` pins the in-process status, so a case cannot pass by every
/// path succeeding, or by all of them failing the same wrong way. Each
/// response in `through_core` must then equal the in-process answer in
/// status, content type and body. Returns that answer.
pub(crate) async fn assert_refusal_parity(
    case: &str,
    expected: StatusCode,
    in_process: Response,
    through_core: impl IntoIterator<Item = Response>,
) -> Answer {
    let in_process = answer(in_process).await;
    assert_eq!(
        in_process.status, expected,
        "{case}: the in-process route must refuse with {expected}: {in_process:?}"
    );
    for (path, response) in through_core.into_iter().enumerate() {
        let through_core = answer(response).await;
        assert_eq!(
            through_core, in_process,
            "{case}: core-backed answer #{path} must equal the in-process answer"
        );
    }
    in_process
}

/// The core's refusal of `method` with `params`, as each router renders a
/// `*_through_core` error: the daemon's router answers
/// `CoreError::into_response`, `zeroclaw-gw` answers through `explain`.
/// Panics when the core accepts the call: a failure case must fail.
pub(crate) async fn core_refusal(core: &CoreCall, method: Method, params: Value) -> [Response; 2] {
    match core.request(method, params).await {
        Ok(result) => panic!("{} must be refused, answered {result}", method.wire_name()),
        Err(error) => [
            error.clone().into_response(),
            crate::preview::explain(error),
        ],
    }
}

/// A JSON request to `uri` whose body is one byte over `limit`, for the
/// body-limit case.
pub(crate) fn oversized(method: HttpMethod, uri: &str, limit: usize) -> Request<Body> {
    let filler = "x".repeat(limit);
    let body = format!("{{\"pad\":\"{filler}\"}}");
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, body.len())
        .body(Body::from(body))
        .expect("a request")
}

/// A JSON request to `uri` whose body ends in an error before it is
/// complete, as from a client that drops the connection mid-body.
pub(crate) fn incomplete(method: HttpMethod, uri: &str) -> Request<Body> {
    let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
        Ok(Bytes::from_static(b"{\"payload\":")),
        Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "the client went away mid-body",
        )),
    ];
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from_stream(tokio_stream::iter(chunks)))
        .expect("a request")
}

// ── Shared state for the cases ──────────────────────────────────

const TOKEN: &str = "zc_refusal_operator";

/// One state both paths read, as under the daemon: the same config, SOP
/// directory, engine, audit logger and pairing.
struct Harness {
    state: AppState,
    core: CoreRpc,
    cancel: tokio_util::sync::CancellationToken,
    _dir: tempfile::TempDir,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Harness {
    /// `sops` saved to disk and loaded into one engine; `None` leaves the
    /// SOP subsystem off on both paths.
    fn new(sops: Option<Vec<Sop>>) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = zeroclaw_config::schema::Config {
            data_dir: dir.path().to_path_buf(),
            config_path: dir.path().join("config.toml"),
            ..Default::default()
        };
        config.gateway.require_pairing = true;
        config.gateway.paired_tokens = vec![TOKEN.into()];
        // The daemon's TUI identity signing key: the in-process connector is
        // a non-local caller, and the core refuses its `initialize` while
        // signing is off.
        std::fs::write(dir.path().join(".secret_key"), "42".repeat(32)).expect("signing key");
        let sops_dir = dir.path().join("sops");
        std::fs::create_dir_all(&sops_dir).expect("a SOP directory");
        config.sop.sops_dir = Some(sops_dir.to_string_lossy().into_owned());
        let sop_parts = sops.map(|sops| {
            for sop in &sops {
                zeroclaw_runtime::sop::save_sop(&sops_dir, sop).expect("save a SOP");
            }
            config.sop.approval = SopApprovalConfig::default();
            let mut engine = SopEngine::new(config.sop.clone())
                .with_approval_broker(Arc::new(ApprovalBroker::disabled()));
            engine.set_sops_for_test(sops);
            let audit = Arc::new(zeroclaw_runtime::sop::SopAuditLogger::new(Arc::new(
                zeroclaw_memory::NoneMemory::new("none"),
            )));
            (Arc::new(Mutex::new(engine)), audit)
        });
        let sessions = Arc::new(zeroclaw_runtime::rpc::session::SessionStore::new(
            16,
            Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
                4, 10, 60,
            )),
        ));
        let mut ctx = RpcContext::for_live_test(config.clone(), sessions);
        assert!(ctx.tui_registry.signing_is_enabled());
        {
            let ctx = Arc::get_mut(&mut ctx).expect("a fresh context is unshared");
            ctx.sop_engine = sop_parts.as_ref().map(|(engine, _)| Arc::clone(engine));
            ctx.sop_audit = sop_parts.as_ref().map(|(_, audit)| Arc::clone(audit));
        }
        let cancel = tokio_util::sync::CancellationToken::new();
        let connector = InprocConnector::new(cancel.clone());
        connector.bind(Arc::clone(&ctx));

        let mut state = crate::api::test_state(config);
        state.pairing = Arc::clone(ctx.auth.pairing());
        state.sop_engine = sop_parts.as_ref().map(|(engine, _)| Arc::clone(engine));
        state.sop_audit = sop_parts.map(|(_, audit)| audit);

        Self {
            state,
            core: CoreRpc::inproc(connector, || true),
            cancel,
            _dir: dir,
        }
    }

    fn headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {TOKEN}")).expect("header"),
        );
        headers
    }

    /// The operator's connection to the core.
    async fn core(&self) -> CoreCall {
        match self.core.access(&Self::headers()).await {
            Ok(CoreAccess::Core(call)) => call,
            Ok(CoreAccess::InProcess) => panic!("served in-process"),
            Err(error) => panic!("no core access: {error:?}"),
        }
    }

    fn engine(&self) -> &Arc<Mutex<SopEngine>> {
        self.state.sop_engine.as_ref().expect("a SOP harness")
    }
}

fn sop(name: &str, steps: Vec<SopStep>) -> Sop {
    Sop {
        name: name.into(),
        description: "refusal parity".into(),
        version: "1.0.0".into(),
        priority: SopPriority::Normal,
        execution_mode: SopExecutionMode::Supervised,
        triggers: vec![SopTrigger::Manual],
        steps,
        cooldown_secs: 0,
        max_concurrent: 4,
        location: None,
        deterministic: false,
        agent: None,
        admission_policy: SopAdmissionPolicy::Parallel,
        max_pending_approvals: 0,
        decision: None,
    }
}

/// One owned step behind a confirmation: a start parks at the gate, with
/// nothing for a driver to do.
fn gated(name: &str, policy: SopAdmissionPolicy) -> Sop {
    let mut sop = sop(
        name,
        vec![SopStep {
            number: 1,
            title: "build".into(),
            requires_confirmation: true,
            kind: SopStepKind::Execute,
            ..SopStep::default()
        }],
    );
    sop.agent = Some("ops".into());
    sop.admission_policy = policy;
    sop
}

/// An `execute` step with no owner: a start from outside an agent turn has
/// no agent to run it.
fn unowned(name: &str) -> Sop {
    sop(
        name,
        vec![SopStep {
            number: 1,
            title: "collect".into(),
            kind: SopStepKind::Execute,
            ..SopStep::default()
        }],
    )
}

/// Park one run of `name` at its gate, directly on the shared engine.
fn park_a_run(harness: &Harness, name: &str) {
    let action = harness
        .engine()
        .lock()
        .unwrap()
        .start_run(
            name,
            SopEvent {
                source: SopTriggerSource::Manual,
                topic: None,
                payload: None,
                timestamp: now_iso8601(),
            },
        )
        .expect("the run starts");
    assert!(
        matches!(action, SopRunAction::WaitApproval { .. }),
        "the run waits at its gate: {action:?}"
    );
}

fn run_body() -> Json<SopRunBody> {
    Json(SopRunBody {
        payload: None,
        dedup_key: None,
    })
}

// ── The cases the split must answer alike ───────────────────────

#[tokio::test]
async fn a_disabled_subsystem_refuses_alike() {
    let harness = Harness::new(None);
    let in_process = handle_sop_runs(
        State(harness.state.clone()),
        Harness::headers(),
        Query(RunsQuery { sop: None }),
    )
    .await;
    let core = harness.core().await;
    let answer = assert_refusal_parity(
        "GET /api/sops/runs with the SOP subsystem off",
        StatusCode::SERVICE_UNAVAILABLE,
        in_process,
        core_refusal(&core, Method::SopsRuns, json!({})).await,
    )
    .await;
    assert_eq!(
        answer.body,
        AnswerBody::Json(json!({ "error": "SOP subsystem not enabled" }))
    );
}

#[tokio::test]
async fn a_missing_resource_refuses_alike() {
    let harness = Harness::new(Some(vec![gated("deploy", SopAdmissionPolicy::Parallel)]));
    let in_process = handle_sop_full(
        State(harness.state.clone()),
        Harness::headers(),
        Path("absent".to_owned()),
    )
    .await;
    let core = harness.core().await;
    assert_refusal_parity(
        "GET /api/sops/{name}/full for a procedure that does not exist",
        StatusCode::NOT_FOUND,
        in_process,
        core_refusal(&core, Method::SopsGet, json!({ "name": "absent" })).await,
    )
    .await;
}

#[tokio::test]
async fn an_unowned_procedure_refuses_alike_and_starts_nothing() {
    let harness = Harness::new(Some(vec![unowned("nightly")]));
    let in_process = handle_sop_run(
        State(harness.state.clone()),
        Harness::headers(),
        Path("nightly".to_owned()),
        run_body(),
    )
    .await;
    let core = harness.core().await;
    assert_refusal_parity(
        "POST /api/sops/{name}/run for a procedure whose step has no owner",
        StatusCode::UNPROCESSABLE_ENTITY,
        in_process,
        core_refusal(&core, Method::SopsRun, json!({ "name": "nightly" })).await,
    )
    .await;
    let guard = harness.engine().lock().unwrap();
    assert!(guard.active_runs().is_empty(), "no path started a run");
    assert!(
        guard.finished_runs(None).is_empty(),
        "no path burned a run id"
    );
}

#[tokio::test]
async fn a_conflict_refuses_alike() {
    let harness = Harness::new(Some(vec![gated("deploy", SopAdmissionPolicy::Parallel)]));
    let existing = gated("deploy", SopAdmissionPolicy::Parallel);
    let in_process = handle_sop_create(
        State(harness.state.clone()),
        Harness::headers(),
        Json(existing.clone()),
    )
    .await;
    let core = harness.core().await;
    assert_refusal_parity(
        "POST /api/sops for a name already taken",
        StatusCode::CONFLICT,
        in_process,
        core_refusal(
            &core,
            Method::SopsCreate,
            json!({ "sop": serde_json::to_value(&existing).unwrap() }),
        )
        .await,
    )
    .await;
}

#[tokio::test]
async fn a_skipped_start_refuses_alike_as_a_conflict() {
    // Drop policy, a one-run approval pool, and that run parked: the next
    // start is dropped, which the route answers as a conflict.
    let mut sop = gated("deploy", SopAdmissionPolicy::Drop);
    sop.max_pending_approvals = 1;
    let harness = Harness::new(Some(vec![sop]));
    park_a_run(&harness, "deploy");
    let in_process = handle_sop_run(
        State(harness.state.clone()),
        Harness::headers(),
        Path("deploy".to_owned()),
        run_body(),
    )
    .await;
    let core = harness.core().await;
    assert_refusal_parity(
        "POST /api/sops/{name}/run dropped by its admission policy",
        StatusCode::CONFLICT,
        in_process,
        core_refusal(&core, Method::SopsRun, json!({ "name": "deploy" })).await,
    )
    .await;
}

#[tokio::test]
async fn a_deferred_start_refuses_alike() {
    // Hold policy with a run already in flight: the next start is deferred.
    let harness = Harness::new(Some(vec![gated("deploy", SopAdmissionPolicy::Hold)]));
    park_a_run(&harness, "deploy");
    let in_process = handle_sop_run(
        State(harness.state.clone()),
        Harness::headers(),
        Path("deploy".to_owned()),
        run_body(),
    )
    .await;
    let core = harness.core().await;
    assert_refusal_parity(
        "POST /api/sops/{name}/run deferred by its admission policy",
        StatusCode::SERVICE_UNAVAILABLE,
        in_process,
        core_refusal(&core, Method::SopsRun, json!({ "name": "deploy" })).await,
    )
    .await;
    let parked = harness.engine().lock().unwrap().active_runs().len();
    assert_eq!(parked, 1, "neither path started a second run");
}

#[tokio::test]
async fn a_missing_config_section_refuses_alike_with_the_config_error() {
    let harness = Harness::new(None);
    let in_process = crate::api_config::handle_get_map_keys(
        State(harness.state.clone()),
        Query(crate::api_config::MapPathQuery {
            path: "nothing.here".into(),
        }),
    )
    .await;
    let core = harness.core().await;
    let answer = assert_refusal_parity(
        "GET /api/config/map-keys for a path with no map-keyed section",
        StatusCode::NOT_FOUND,
        in_process,
        core_refusal(
            &core,
            Method::ConfigMapKeys,
            json!({ "path": "nothing.here" }),
        )
        .await,
    )
    .await;
    let AnswerBody::Json(body) = answer.body else {
        panic!("a JSON config error")
    };
    assert_eq!(body["code"], "path_not_found");
    assert_eq!(body["path"], "nothing.here");
}

#[tokio::test]
async fn an_unknown_config_property_refuses_alike_with_the_config_error() {
    let harness = Harness::new(None);
    let in_process = crate::api_config::handle_prop_get(
        State(harness.state.clone()),
        Query(crate::api_config::PropQuery {
            path: "gateway.no_such_field".into(),
        }),
    )
    .await;
    let core = harness.core().await;
    assert_refusal_parity(
        "GET /api/config/prop for a path the schema does not define",
        StatusCode::NOT_FOUND,
        in_process,
        core_refusal(
            &core,
            Method::ConfigGet,
            json!({ "prop": "gateway.no_such_field" }),
        )
        .await,
    )
    .await;
}

#[tokio::test]
async fn a_disabled_session_store_answers_the_listing_route_alike() {
    // The listing answers an empty 200 rather than refusing; through the core
    // that is the `disabled` refusal, which the route turns into its body.
    let harness = Harness::new(None);
    let mut state = harness.state.clone();
    state.session_backend = None;
    let in_process = crate::api::handle_api_sessions_list(State(state), Harness::headers())
        .await
        .into_response();
    let core = harness.core().await;
    let through_core = crate::api::api_sessions_list_through_core(&core)
        .await
        .unwrap_or_else(IntoResponse::into_response);
    assert_refusal_parity(
        "GET /api/sessions with session persistence off",
        StatusCode::OK,
        in_process,
        [through_core],
    )
    .await;
}

// ── The body cases, as the production layers answer them ────────

#[tokio::test]
async fn the_body_builders_trip_the_production_layers() {
    use tower::ServiceExt as _;

    let router = || {
        axum::Router::new()
            .route(
                "/echo",
                axum::routing::post(|Json(body): Json<Value>| async move { Json(body) }),
            )
            .layer(tower_http::limit::RequestBodyLimitLayer::new(
                crate::MAX_BODY_SIZE,
            ))
    };
    let oversized = answer(
        router()
            .oneshot(oversized(HttpMethod::POST, "/echo", crate::MAX_BODY_SIZE))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(oversized.status, StatusCode::PAYLOAD_TOO_LARGE);
    let incomplete = answer(
        router()
            .oneshot(incomplete(HttpMethod::POST, "/echo"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(incomplete.status, StatusCode::BAD_REQUEST);
    assert!(
        matches!(incomplete.body, AnswerBody::Bytes(_)),
        "a framework rejection is plain text: {incomplete:?}"
    );
}

#[test]
fn every_reason_answers_with_a_client_error_or_unavailable() {
    use zeroclaw_rpc_proto::error_reasons::RefusalReason;

    let mut seen = HashMap::new();
    for reason in RefusalReason::ALL {
        let status = crate::core_rpc::refusal_status(*reason);
        assert!(
            status.is_client_error() || status == StatusCode::SERVICE_UNAVAILABLE,
            "{reason:?} answers {status}"
        );
        seen.insert(reason.as_str(), status.as_u16());
    }
    assert_eq!(
        seen,
        HashMap::from([
            ("not_found", 404),
            ("unowned", 422),
            ("conflict", 409),
            ("disabled", 503),
            ("deferred", 503),
            ("capacity", 429),
            ("forbidden", 403),
            ("invalid", 400),
            ("blocked", 422),
            ("malformed", 422),
            ("entry_exceeds_max_bytes", 400),
        ])
    );
}
