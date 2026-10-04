//! Parity of the routes served through the core with their in-process bodies.
//!
//! Each test runs a route's handler twice over one shared state, once
//! in-process and once through the daemon's real in-process connector, and
//! requires the same status and body. The state the two paths read (cost
//! tracker, TUI registry, event history, pairing, SOP directory and engine)
//! is the same instance, as it is under the daemon.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use zeroclaw_config::cost::{CostTracker, TokenUsage};
use zeroclaw_config::schema::{ApprovalGroupConfig, ApprovalPolicyConfig, SopApprovalConfig};
use zeroclaw_rpc_proto::types::CLIENT_KIND_GATEWAY;
use zeroclaw_runtime::rpc::context::RpcContext;
use zeroclaw_runtime::rpc::inproc::InprocConnector;
use zeroclaw_runtime::rpc::tui_identity::TuiEntry;
use zeroclaw_runtime::sop::approval::ApprovalBroker;
use zeroclaw_runtime::sop::engine::{SopEngine, now_iso8601};
use zeroclaw_runtime::sop::types::{
    Sop, SopAdmissionPolicy, SopEvent, SopExecutionMode, SopPriority, SopRunAction, SopRunStatus,
    SopStep, SopStepKind, SopTrigger, SopTriggerSource,
};

use crate::AppState;
use crate::api::{CostQuery, handle_api_cost, handle_api_health, handle_api_tuis};
use crate::api_sop_author::{
    GraphDraftRequest, ParamOptionsBody, RunsQuery, SopRenameBody, SopRunBody, WireDraftRequest,
    handle_sop_create, handle_sop_decide, handle_sop_delete, handle_sop_full, handle_sop_graph,
    handle_sop_graph_draft, handle_sop_rename, handle_sop_run, handle_sop_run_overlay,
    handle_sop_runs, handle_sop_save, handle_sop_trigger_sources, handle_sop_wire_draft,
    handle_sops_list, handle_tools_param_options,
};
use crate::core_rpc::{CoreAccess, CoreRpc};
use crate::sse::{EventBuffer, handle_events_history};

const TOKEN: &str = "zc_parity_operator";
/// A second paired device, for a quorum of two.
const SECOND: &str = "zc_parity_second";
/// A paired device no approval group lists.
const OUTSIDER: &str = "zc_parity_outsider";

struct Harness {
    state: AppState,
    ctx: Arc<RpcContext>,
    core: CoreRpc,
    cancel: tokio_util::sync::CancellationToken,
    _dir: tempfile::TempDir,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// The SOP state both paths read: these procedures in one directory, one
/// engine holding them under this approval config, and one audit logger.
type SopSetup = (Vec<Sop>, SopApprovalConfig);

impl Harness {
    fn new(cost_tracker: Option<Arc<CostTracker>>) -> Self {
        Self::build(cost_tracker, None)
    }

    fn with_sops(sops: Vec<Sop>, approval: SopApprovalConfig) -> Self {
        Self::build(None, Some((sops, approval)))
    }

    fn build(cost_tracker: Option<Arc<CostTracker>>, sops: Option<SopSetup>) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = zeroclaw_config::schema::Config {
            data_dir: dir.path().to_path_buf(),
            config_path: dir.path().join("config.toml"),
            ..Default::default()
        };
        config.gateway.require_pairing = true;
        config.gateway.paired_tokens = vec![TOKEN.into(), SECOND.into(), OUTSIDER.into()];
        // The daemon's TUI identity signing key. The in-process connector is
        // a non-local caller, and the core refuses its `initialize` while
        // signing is off.
        std::fs::write(dir.path().join(".secret_key"), "42".repeat(32)).expect("signing key");
        let sop_parts = sops.map(|(sops, approval)| {
            let sops_dir = dir.path().join("sops");
            for sop in &sops {
                zeroclaw_runtime::sop::save_sop(&sops_dir, sop).expect("save a SOP");
            }
            config.sop.sops_dir = Some(sops_dir.to_string_lossy().into_owned());
            config.sop.approval = approval;
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
        let history = Arc::new(EventBuffer::new(16));
        let mut ctx = RpcContext::for_live_test(config.clone(), sessions);
        assert!(ctx.tui_registry.signing_is_enabled());
        {
            let ctx = Arc::get_mut(&mut ctx).expect("a fresh context is unshared");
            ctx.cost_tracker = cost_tracker.clone();
            ctx.event_history = Some(Arc::clone(&history));
            ctx.sop_engine = sop_parts.as_ref().map(|(engine, _)| Arc::clone(engine));
            ctx.sop_audit = sop_parts.as_ref().map(|(_, audit)| Arc::clone(audit));
        }
        let cancel = tokio_util::sync::CancellationToken::new();
        let connector = InprocConnector::new(cancel.clone());
        connector.bind(Arc::clone(&ctx));

        let mut state = crate::api::test_state(config);
        state.pairing = Arc::clone(ctx.auth.pairing());
        state.cost_tracker = cost_tracker;
        state.event_buffer = history;
        state.tui_registry = Some(Arc::clone(&ctx.tui_registry));
        state.sop_engine = sop_parts.as_ref().map(|(engine, _)| Arc::clone(engine));
        state.sop_audit = sop_parts.map(|(_, audit)| audit);

        Self {
            state,
            ctx,
            core: CoreRpc::inproc(connector, || true),
            cancel,
            _dir: dir,
        }
    }

    fn headers() -> HeaderMap {
        Self::headers_for(TOKEN)
    }

    fn headers_for(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).expect("header"),
        );
        headers
    }

    /// A request's access through the core, bound to the operator's bearer.
    async fn through_core(&self) -> CoreAccess {
        self.through_core_as(TOKEN).await
    }

    /// A request's access through the core, bound to `token`.
    async fn through_core_as(&self, token: &str) -> CoreAccess {
        match self.core.access(&Self::headers_for(token)).await {
            Ok(access @ CoreAccess::Core(_)) => access,
            Ok(CoreAccess::InProcess) => panic!("served in-process"),
            Err(error) => panic!("no core access: {error:?}"),
        }
    }

    fn engine(&self) -> &Arc<Mutex<SopEngine>> {
        self.state.sop_engine.as_ref().expect("a SOP harness")
    }
}

async fn body_of(response: Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let body = serde_json::from_slice(&bytes).expect("JSON body");
    (status, body)
}

/// Both paths answer with the same status and body; returns the body.
fn assert_same(in_process: (StatusCode, Value), through_core: (StatusCode, Value)) -> Value {
    assert_eq!(in_process.0, StatusCode::OK, "{}", in_process.1);
    assert_eq!(
        through_core, in_process,
        "the core path must answer exactly as the in-process body"
    );
    in_process.1
}

#[tokio::test]
async fn api_health_through_the_core_matches_the_in_process_body() {
    let harness = Harness::new(None);
    zeroclaw_runtime::health::mark_component_ok("gw-parity-health");

    // The snapshot is process-wide and time-varying: its own `updated_at`
    // is the moment it was taken, and parallel tests may touch components.
    // Compare against an in-process body that did not move across the core
    // call, ignoring only that timestamp.
    let strip = |(status, mut body): (StatusCode, Value)| {
        if let Some(health) = body.get_mut("health").and_then(Value::as_object_mut) {
            health.remove("updated_at");
        }
        (status, body)
    };
    for attempt in 0.. {
        let before = strip(
            body_of(
                handle_api_health(
                    State(harness.state.clone()),
                    Harness::headers(),
                    CoreAccess::InProcess,
                )
                .await
                .into_response(),
            )
            .await,
        );
        let core = strip(
            body_of(
                handle_api_health(
                    State(harness.state.clone()),
                    Harness::headers(),
                    harness.through_core().await,
                )
                .await
                .into_response(),
            )
            .await,
        );
        let after = strip(
            body_of(
                handle_api_health(
                    State(harness.state.clone()),
                    Harness::headers(),
                    CoreAccess::InProcess,
                )
                .await
                .into_response(),
            )
            .await,
        );
        if before != after {
            assert!(attempt < 20, "the health snapshot never held still");
            continue;
        }
        let body = assert_same(before, core);
        assert!(body["health"].get("process").is_none());
        assert!(body["health"]["components"]["gw-parity-health"].is_object());
        break;
    }
}

#[tokio::test]
async fn api_tuis_through_the_core_matches_the_in_process_body() {
    let harness = Harness::new(None);
    harness.ctx.tui_registry.register(TuiEntry {
        tui_id: "tui_parity".into(),
        connected_at: chrono::Utc::now(),
        peer_label: "unix:parity".into(),
        transport: "unix".into(),
        env: HashMap::new(),
        client_kind: None,
    });

    // As in production, the gateway's own core connection exists (and the
    // core has registered it, as a gateway's) before either body is read.
    let through_core = harness.through_core().await;
    let own = harness
        .ctx
        .tui_registry
        .list()
        .into_iter()
        .find(|tui| tui.peer_label == zeroclaw_runtime::rpc::inproc::PEER_LABEL)
        .expect("the core registers the gateway's connection");
    assert_eq!(own.client_kind.as_deref(), Some(CLIENT_KIND_GATEWAY));
    let in_process = body_of(
        handle_api_tuis(
            State(harness.state.clone()),
            Harness::headers(),
            CoreAccess::InProcess,
        )
        .await
        .into_response(),
    )
    .await;
    let core = body_of(
        handle_api_tuis(
            State(harness.state.clone()),
            Harness::headers(),
            through_core,
        )
        .await
        .into_response(),
    )
    .await;
    let body = assert_same(in_process, core);
    let tuis = body["tuis"].as_array().expect("tuis");
    assert!(tuis.iter().any(|tui| tui["tui_id"] == "tui_parity"));
    assert!(
        tuis.iter().all(|tui| tui["tui_id"] != own.tui_id.as_str()),
        "the gateway's own connections are not terminals: {body}"
    );
    assert!(
        tuis.iter().all(|tui| tui.get("client_kind").is_none()),
        "the route's rows carry no client kind: {body}"
    );
}

#[tokio::test]
async fn events_history_through_the_core_matches_the_in_process_body() {
    let harness = Harness::new(None);
    harness.state.event_buffer.push(json!({
        "source": "observability",
        "type": "agent_start",
        "model": "parity-model",
    }));
    harness.state.event_buffer.push(json!({
        "type": "llm_request",
        "session_id": "withheld-from-the-global-view",
    }));

    let in_process = body_of(
        handle_events_history(
            State(harness.state.clone()),
            Harness::headers(),
            CoreAccess::InProcess,
        )
        .await
        .into_response(),
    )
    .await;
    let core = body_of(
        handle_events_history(
            State(harness.state.clone()),
            Harness::headers(),
            harness.through_core().await,
        )
        .await
        .into_response(),
    )
    .await;
    let body = assert_same(in_process, core);
    assert_eq!(body["events"].as_array().expect("events").len(), 1);
}

fn usage_at(model: &str, cost: f64, when: chrono::DateTime<chrono::Utc>) -> TokenUsage {
    let mut usage = TokenUsage::new(model, 1_000, 500, 0, 0.0, 0.0, 0.0);
    usage.cost_usd = cost;
    usage.timestamp = when;
    usage
}

fn cost_tracker() -> (Arc<CostTracker>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = zeroclaw_config::schema::CostConfig {
        enabled: true,
        track_per_agent: true,
        ..Default::default()
    };
    let tracker = Arc::new(CostTracker::new(config, dir.path()).expect("tracker"));
    let now = chrono::Utc::now();
    tracker
        .record_usage_with_agent(usage_at("parity/today", 0.25, now), Some("alpha"))
        .expect("record");
    // A record from well before the current month: only an all-time
    // summary includes it.
    tracker
        .record_usage_with_agent(
            usage_at("parity/last-year", 1.5, now - chrono::Duration::days(400)),
            Some("beta"),
        )
        .expect("record");
    (tracker, dir)
}

#[tokio::test]
async fn api_cost_through_the_core_matches_the_in_process_body() {
    let (tracker, _dir) = cost_tracker();
    let harness = Harness::new(Some(tracker));
    let recent = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
    let cases: Vec<(&str, CostQuery)> = vec![
        ("no bounds", CostQuery::default()),
        (
            "agent",
            CostQuery {
                agent: Some("beta".into()),
                ..Default::default()
            },
        ),
        (
            "blank agent",
            CostQuery {
                agent: Some(String::new()),
                ..Default::default()
            },
        ),
        (
            "unknown agent",
            CostQuery {
                agent: Some("nobody".into()),
                ..Default::default()
            },
        ),
        (
            "agent ignores the window",
            CostQuery {
                agent: Some("alpha".into()),
                from: Some(recent.clone()),
                to: None,
            },
        ),
        (
            "from",
            CostQuery {
                from: Some(recent.clone()),
                ..Default::default()
            },
        ),
        (
            "to",
            CostQuery {
                to: Some(recent.clone()),
                ..Default::default()
            },
        ),
        (
            "unparsable from reads as absent",
            CostQuery {
                from: Some("yesterday".into()),
                ..Default::default()
            },
        ),
        (
            "unparsable from, valid to",
            CostQuery {
                from: Some("yesterday".into()),
                to: Some(recent.clone()),
                agent: None,
            },
        ),
    ];

    for (case, query) in cases {
        let in_process = body_of(
            handle_api_cost(
                State(harness.state.clone()),
                Harness::headers(),
                Query(query.clone()),
                CoreAccess::InProcess,
            )
            .await
            .into_response(),
        )
        .await;
        let core = body_of(
            handle_api_cost(
                State(harness.state.clone()),
                Harness::headers(),
                Query(query),
                harness.through_core().await,
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(core, in_process, "{case}");
        assert_eq!(in_process.0, StatusCode::OK, "{case}: {}", in_process.1);
        if case == "no bounds" {
            assert!(
                in_process.1["cost"]["by_model"]
                    .get("parity/last-year")
                    .is_some(),
                "the unbounded summary is all-time: {}",
                in_process.1
            );
        }
    }
}

#[tokio::test]
async fn api_cost_with_tracking_disabled_matches_the_in_process_body() {
    let harness = Harness::new(None);
    let in_process = body_of(
        handle_api_cost(
            State(harness.state.clone()),
            Harness::headers(),
            Query(CostQuery::default()),
            CoreAccess::InProcess,
        )
        .await
        .into_response(),
    )
    .await;
    let core = body_of(
        handle_api_cost(
            State(harness.state.clone()),
            Harness::headers(),
            Query(CostQuery::default()),
            harness.through_core().await,
        )
        .await
        .into_response(),
    )
    .await;
    let body = assert_same(in_process, core);
    assert_eq!(body["cost"]["request_count"], 0);
}

// ── SOP authoring ────────────────────────────────────────────────

fn sop(name: &str, mode: SopExecutionMode, steps: Vec<SopStep>) -> Sop {
    Sop {
        name: name.into(),
        description: "parity".into(),
        version: "1.0.0".into(),
        priority: SopPriority::Normal,
        execution_mode: mode,
        triggers: vec![SopTrigger::Manual],
        steps,
        cooldown_secs: 0,
        max_concurrent: 4,
        location: None,
        deterministic: mode == SopExecutionMode::Deterministic,
        agent: None,
        admission_policy: SopAdmissionPolicy::Parallel,
        max_pending_approvals: 0,
        decision: None,
    }
}

/// One deterministic checkpoint the `prod` policy gates: approving it ends
/// the run, so a decision's overlay does not depend on a driver's timing.
fn checkpoint_sop() -> Sop {
    sop(
        "release",
        SopExecutionMode::Deterministic,
        vec![SopStep {
            number: 1,
            title: "sign off".into(),
            kind: SopStepKind::Checkpoint,
            policy: Some("prod".into()),
            ..SopStep::default()
        }],
    )
}

/// Two owned steps, the first gated by a confirmation: starting it parks
/// the run at the gate, with nothing to drive.
fn gated_sop() -> Sop {
    let mut sop = sop(
        "deploy",
        SopExecutionMode::Supervised,
        vec![
            SopStep {
                number: 1,
                title: "build".into(),
                requires_confirmation: true,
                kind: SopStepKind::Execute,
                ..SopStep::default()
            },
            SopStep {
                number: 2,
                title: "ship".into(),
                kind: SopStepKind::Execute,
                ..SopStep::default()
            },
        ],
    );
    sop.agent = Some("ops".into());
    sop
}

/// An `execute` step with no owner: a headless start has no agent to run it.
fn unowned_sop() -> Sop {
    sop(
        "nightly",
        SopExecutionMode::Auto,
        vec![SopStep {
            number: 1,
            title: "collect".into(),
            kind: SopStepKind::Execute,
            ..SopStep::default()
        }],
    )
}

/// The `prod` policy: the `release` group, whose members are these paired
/// devices' `http:` subjects, and `quorum` of them.
fn approval(quorum: u32, members: &[&str]) -> SopApprovalConfig {
    let members = members
        .iter()
        .map(|token| {
            format!(
                "http:{}",
                zeroclaw_runtime::security::pairing::PairingGuard::token_hash(token)
            )
        })
        .collect();
    SopApprovalConfig {
        groups: HashMap::from([("release".to_string(), ApprovalGroupConfig { members })]),
        policies: HashMap::from([(
            "prod".to_string(),
            ApprovalPolicyConfig {
                required_group: Some("release".into()),
                quorum,
                request_route: None,
                escalation_route: None,
            },
        )]),
    }
}

fn start(harness: &Harness, name: &str) -> String {
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
    match action {
        SopRunAction::WaitApproval { run_id, .. } | SopRunAction::CheckpointWait { run_id, .. } => {
            run_id
        }
        other => panic!("expected the run to wait at its gate, got {other:?}"),
    }
}

fn run_status(harness: &Harness, run_id: &str) -> Option<SopRunStatus> {
    harness
        .engine()
        .lock()
        .unwrap()
        .get_run(run_id)
        .map(|run| run.status)
}

/// A route's answer in-process and through the core, as `(status, body)`.
async fn both<F, Fut>(harness: &Harness, route: F) -> ((StatusCode, Value), (StatusCode, Value))
where
    F: Fn(CoreAccess) -> Fut,
    Fut: std::future::Future<Output = Response>,
{
    let in_process = body_of(route(CoreAccess::InProcess).await).await;
    let core = body_of(route(harness.through_core().await).await).await;
    (in_process, core)
}

#[tokio::test]
async fn sop_reads_through_the_core_match_the_in_process_bodies() {
    let harness = Harness::with_sops(vec![gated_sop(), checkpoint_sop()], approval(1, &[TOKEN]));
    let run_id = start(&harness, "deploy");
    let state = || State(harness.state.clone());
    let headers = Harness::headers;

    let (a, b) = both(&harness, |access| {
        handle_sops_list(state(), headers(), access)
    })
    .await;
    let listed = assert_same(a, b);
    assert_eq!(listed["sops"].as_array().map(Vec::len), Some(2), "{listed}");

    let (a, b) = both(&harness, |access| {
        handle_sop_full(state(), headers(), Path("deploy".into()), access)
    })
    .await;
    assert_eq!(assert_same(a, b)["name"], "deploy");

    let (a, b) = both(&harness, |access| {
        handle_sop_graph(state(), headers(), Path("deploy".into()), access)
    })
    .await;
    assert_same(a, b);

    let (a, b) = both(&harness, |access| {
        handle_sop_trigger_sources(state(), headers(), access)
    })
    .await;
    assert_same(a, b);

    let (a, b) = both(&harness, |access| {
        let query = RunsQuery {
            sop: Some("deploy".into()),
        };
        handle_sop_runs(state(), headers(), Query(query), access)
    })
    .await;
    let runs = assert_same(a, b);
    assert_eq!(runs["runs"][0]["run_id"], run_id.as_str(), "{runs}");

    let (a, b) = both(&harness, |access| {
        handle_sop_run_overlay(
            state(),
            headers(),
            Path(("deploy".into(), run_id.clone())),
            access,
        )
    })
    .await;
    assert_eq!(assert_same(a, b)["waiting"], true);

    let (a, b) = both(&harness, |access| {
        let request = WireDraftRequest {
            sop: gated_sop(),
            edit: serde_json::from_value(
                json!({ "op": "connect", "from": 1, "to": 2, "role": "failure" }),
            )
            .expect("a wire edit"),
        };
        handle_sop_wire_draft(state(), headers(), access, Json(request))
    })
    .await;
    let wired = assert_same(a, b);
    assert!(wired["graph"].is_object(), "{wired}");

    let (a, b) = both(&harness, |access| {
        let request = GraphDraftRequest { sop: gated_sop() };
        handle_sop_graph_draft(state(), headers(), access, Json(request))
    })
    .await;
    assert_same(a, b);

    for domain in ["tool_names", "agent_aliases"] {
        let (a, b) = both(&harness, |access| {
            let body = ParamOptionsBody {
                domain: serde_json::from_value(json!(domain)).expect("a domain"),
                agent: None,
                args: Value::Null,
            };
            handle_tools_param_options(state(), headers(), access, Json(body))
        })
        .await;
        assert_same(a, b);
    }
}

/// Each authoring write answers the same body either way, and leaves the
/// same definition on disk. Every write is undone before the core repeats
/// it, so both paths act on the same state.
#[tokio::test]
async fn sop_writes_through_the_core_match_the_in_process_bodies() {
    let harness = Harness::with_sops(vec![gated_sop()], approval(1, &[TOKEN]));
    let state = || State(harness.state.clone());
    let headers = Harness::headers;
    let sops_dir = std::path::PathBuf::from(
        harness
            .state
            .config
            .read()
            .sop
            .sops_dir
            .clone()
            .expect("a SOP directory"),
    );
    let definition =
        |name: &str| std::fs::read_to_string(sops_dir.join(name).join("SOP.toml")).ok();
    let mut drafted = gated_sop();
    drafted.name = "drafted".into();

    let in_process = body_of(
        handle_sop_create(
            state(),
            headers(),
            CoreAccess::InProcess,
            Json(drafted.clone()),
        )
        .await,
    )
    .await;
    let written = definition("drafted");
    assert!(written.is_some(), "the create wrote a definition");
    zeroclaw_runtime::sop::delete_sop_typed(&sops_dir, "drafted").unwrap();
    let core = body_of(
        handle_sop_create(
            state(),
            headers(),
            harness.through_core().await,
            Json(drafted.clone()),
        )
        .await,
    )
    .await;
    assert_eq!(assert_same(in_process, core)["created"], "drafted");
    assert_eq!(definition("drafted"), written);

    let mut edited = drafted.clone();
    edited.description = "edited".into();
    edited.name = String::new();
    let (a, b) = both(&harness, |access| {
        handle_sop_save(
            state(),
            headers(),
            Path("drafted".into()),
            access,
            Json(edited.clone()),
        )
    })
    .await;
    assert_eq!(assert_same(a, b)["saved"], "drafted");
    assert!(definition("drafted").is_some_and(|toml| toml.contains("edited")));

    // A body naming another SOP than its URL is refused alike.
    let (a, b) = both(&harness, |access| {
        handle_sop_save(
            state(),
            headers(),
            Path("drafted".into()),
            access,
            Json(gated_sop()),
        )
    })
    .await;
    assert_eq!(a.0, StatusCode::BAD_REQUEST, "{}", a.1);
    assert_eq!(a, b);

    let rename = |to: &str, access| {
        let to = to.to_owned();
        let from = if to == "renamed" {
            "drafted"
        } else {
            "renamed"
        };
        handle_sop_rename(
            state(),
            headers(),
            Path(from.into()),
            access,
            Json(SopRenameBody { to }),
        )
    };
    let in_process = body_of(rename("renamed", CoreAccess::InProcess).await).await;
    body_of(rename("drafted", CoreAccess::InProcess).await).await;
    let core = body_of(rename("renamed", harness.through_core().await).await).await;
    assert_eq!(assert_same(in_process, core)["renamed"], "renamed");

    let in_process = body_of(
        handle_sop_delete(
            state(),
            headers(),
            Path("renamed".into()),
            CoreAccess::InProcess,
        )
        .await,
    )
    .await;
    let mut restored = drafted.clone();
    restored.name = "renamed".into();
    body_of(handle_sop_create(state(), headers(), CoreAccess::InProcess, Json(restored)).await)
        .await;
    let core = body_of(
        handle_sop_delete(
            state(),
            headers(),
            Path("renamed".into()),
            harness.through_core().await,
        )
        .await,
    )
    .await;
    assert_eq!(assert_same(in_process, core)["deleted"], "renamed");
    assert_eq!(definition("renamed"), None);
}

/// A manual run starts alike, and a procedure the headless driver cannot run
/// is refused before it starts, with the same reason, on both paths.
#[tokio::test]
async fn sop_run_through_the_core_matches_and_refuses_an_unowned_procedure() {
    let harness = Harness::with_sops(vec![gated_sop(), unowned_sop()], approval(1, &[TOKEN]));
    let state = || State(harness.state.clone());
    let headers = Harness::headers;
    let run = |name: &str, access| {
        handle_sop_run(
            state(),
            headers(),
            Path(name.to_owned()),
            access,
            Json(SopRunBody {
                payload: None,
                dedup_key: None,
            }),
        )
    };

    let in_process = body_of(run("deploy", CoreAccess::InProcess).await).await;
    let core = body_of(run("deploy", harness.through_core().await).await).await;
    assert_eq!(in_process.0, StatusCode::OK, "{}", in_process.1);
    assert_eq!(core.0, StatusCode::OK, "{}", core.1);
    for (status, body) in [&in_process, &core] {
        let run_id = body["run_id"].as_str().expect("a run id");
        assert_eq!(
            body.as_object().map(|fields| fields.len()),
            Some(1),
            "{status}: {body}"
        );
        assert_eq!(
            run_status(&harness, run_id),
            Some(SopRunStatus::WaitingApproval)
        );
    }
    assert_ne!(in_process.1["run_id"], core.1["run_id"]);

    let in_process = body_of(run("nightly", CoreAccess::InProcess).await).await;
    let core = body_of(run("nightly", harness.through_core().await).await).await;
    assert_eq!(
        in_process.0,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        in_process.1
    );
    // The core refuses with its invalid-params code, which the gateway answers
    // 400, carrying the same reason.
    assert_eq!(core.0, StatusCode::BAD_REQUEST, "{}", core.1);
    assert_eq!(core.1["code"], "invalid_params");
    assert_eq!(core.1["error"], in_process.1["error"]);
    let started = harness
        .engine()
        .lock()
        .unwrap()
        .active_runs()
        .values()
        .filter(|run| run.sop_name == "nightly")
        .count();
    assert_eq!(started, 0, "neither path started the unowned procedure");
}

/// A decision through the core is the caller's: a paired device decides as
/// the same `http:` subject the in-process route derives, so the same group
/// members count, the same outsider is refused (and keeps its credential), a
/// vote short of the quorum answers `202` with the overlay, and the vote that
/// meets it answers `200`.
#[tokio::test]
async fn sop_decide_through_the_core_matches_the_in_process_answers() {
    let harness = Harness::with_sops(vec![checkpoint_sop()], approval(2, &[TOKEN, SECOND]));
    let in_process_run = start(&harness, "release");
    let core_run = start(&harness, "release");
    let decide = |token: &'static str, run_id: &str, access| {
        handle_sop_decide(
            State(harness.state.clone()),
            Harness::headers_for(token),
            Path(("release".into(), run_id.to_owned())),
            access,
            Json(json!("approve")),
        )
    };
    // The two runs differ only in their ids.
    let same_run = |(status, body): (StatusCode, Value)| {
        let body = body.to_string().replace(&core_run, &in_process_run);
        (status, serde_json::from_str::<Value>(&body).unwrap())
    };

    let in_process = body_of(decide(OUTSIDER, &in_process_run, CoreAccess::InProcess).await).await;
    let core =
        body_of(decide(OUTSIDER, &core_run, harness.through_core_as(OUTSIDER).await).await).await;
    assert_eq!(in_process.0, StatusCode::FORBIDDEN, "{}", in_process.1);
    assert_eq!(core.0, StatusCode::FORBIDDEN, "{}", core.1);
    assert_eq!(core.1["code"], "forbidden", "{}", core.1);
    for run_id in [&in_process_run, &core_run] {
        assert_eq!(
            run_status(&harness, run_id),
            Some(SopRunStatus::PausedCheckpoint)
        );
    }
    // A refusal on the merits leaves the outsider signed in.
    let after = body_of(
        handle_sops_list(
            State(harness.state.clone()),
            Harness::headers_for(OUTSIDER),
            harness.through_core_as(OUTSIDER).await,
        )
        .await,
    )
    .await;
    assert_eq!(after.0, StatusCode::OK, "{}", after.1);

    let in_process = body_of(decide(TOKEN, &in_process_run, CoreAccess::InProcess).await).await;
    let core = body_of(decide(TOKEN, &core_run, harness.through_core().await).await).await;
    assert_eq!(in_process.0, StatusCode::ACCEPTED, "{}", in_process.1);
    assert_eq!(same_run(core), in_process, "a pending vote answers alike");
    assert_eq!(in_process.1["status"], "paused_checkpoint");

    let in_process = body_of(decide(SECOND, &in_process_run, CoreAccess::InProcess).await).await;
    let core =
        body_of(decide(SECOND, &core_run, harness.through_core_as(SECOND).await).await).await;
    assert_eq!(in_process.0, StatusCode::OK, "{}", in_process.1);
    assert_eq!(
        same_run(core),
        in_process,
        "the vote meeting the quorum answers alike"
    );
    for run_id in [&in_process_run, &core_run] {
        assert_ne!(
            run_status(&harness, run_id),
            Some(SopRunStatus::PausedCheckpoint),
            "the quorum cleared the gate"
        );
    }
}

// ── Run admission at the commit ──────────────────────────────────

/// A decision model that parks every question until the test releases it, so
/// a test can change the caller's authority or the procedure while a run waits
/// on it.
struct ParkedDecision {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl zeroclaw_runtime::sop::decision::DecisionModel for ParkedDecision {
    fn id(&self) -> &str {
        "parked"
    }

    async fn ask(
        &self,
        _: Value,
        _: std::collections::BTreeMap<String, zeroclaw_runtime::sop::decision::Question>,
    ) -> anyhow::Result<zeroclaw_runtime::sop::decision::Answers> {
        self.entered.notify_one();
        self.release.notified().await;
        serde_json::from_value(
            json!({ "answers": { "start_sop": { "type": "noul", "noul": 1.0 } } }),
        )
        .map_err(Into::into)
    }
}

impl Harness {
    /// Load `deploy` behind a decision gate answered by a [`ParkedDecision`].
    fn park_decisions(&self) -> Arc<ParkedDecision> {
        let model = Arc::new(ParkedDecision {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let mut procedure = gated_sop();
        procedure.decision =
            Some(serde_json::from_value(json!({ "model": "parked", "gate": "start?" })).unwrap());
        let mut engine = SopEngine::new(self.state.config.read().sop.clone()).with_decision_models(
            HashMap::from([(
                "parked".to_string(),
                Arc::clone(&model) as Arc<dyn zeroclaw_runtime::sop::decision::DecisionModel>,
            )]),
        );
        engine.set_sops_for_test(vec![procedure]);
        *self.engine().lock().unwrap() = engine;
        model
    }

    /// A request's access: through the core, or the in-process body.
    async fn access(&self, through_core: bool) -> CoreAccess {
        if through_core {
            self.through_core().await
        } else {
            CoreAccess::InProcess
        }
    }

    fn active_runs(&self) -> usize {
        self.engine().lock().unwrap().active_runs().len()
    }

    /// Start `POST /api/sops/deploy/run` on its own task, and return once it
    /// is parked in the decision model.
    async fn run_parked(
        &self,
        model: &ParkedDecision,
        access: CoreAccess,
        dedup_key: Option<&str>,
    ) -> tokio::task::JoinHandle<(StatusCode, Value)> {
        let state = self.state.clone();
        let body = SopRunBody {
            payload: None,
            dedup_key: dedup_key.map(str::to_string),
        };
        let task = zeroclaw_spawn::spawn!(async move {
            body_of(
                handle_sop_run(
                    State(state),
                    Harness::headers(),
                    Path("deploy".into()),
                    access,
                    Json(body),
                )
                .await,
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), model.entered.notified())
            .await
            .expect("the run reaches the decision model");
        task
    }
}

/// The procedure is reloaded, while a run waits on the decision model, into
/// one the headless driver cannot run. The run is refused at its commit on
/// either path, and starts nothing.
#[tokio::test]
async fn a_run_is_refused_when_its_procedure_changes_during_the_decision_wait() {
    for through_core in [false, true] {
        let harness = Harness::with_sops(vec![gated_sop()], approval(1, &[TOKEN]));
        let model = harness.park_decisions();
        let task = harness
            .run_parked(&model, harness.access(through_core).await, None)
            .await;
        assert_eq!(harness.active_runs(), 0, "parked before the commit");

        let mut unowned = gated_sop();
        unowned.agent = None;
        let dir =
            std::path::PathBuf::from(harness.state.config.read().sop.sops_dir.clone().unwrap());
        zeroclaw_runtime::sop::save_existing_sop_typed(&dir, &unowned).unwrap();
        harness.engine().lock().unwrap().reload(harness._dir.path());
        assert!(
            zeroclaw_runtime::sop::headless_ownership_refusal(
                harness.engine().lock().unwrap().get_sop("deploy").unwrap()
            )
            .is_some()
        );

        model.release.notify_one();
        let (status, body) = task.await.unwrap();
        assert!(!status.is_success(), "core={through_core}: {status} {body}");
        if !through_core {
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        }
        assert_eq!(
            harness.active_runs(),
            0,
            "core={through_core}: nothing started"
        );
    }
}

/// The caller's pairing is revoked while its run waits on the decision model.
/// The run is refused at its commit on either path, and so is a second run
/// that would otherwise coalesce onto an active one.
#[tokio::test]
async fn a_run_is_refused_when_the_credential_is_revoked_during_the_decision_wait() {
    for through_core in [false, true] {
        let harness = Harness::with_sops(vec![gated_sop()], approval(1, &[TOKEN]));
        let model = harness.park_decisions();
        // A run that starts, and keeps a shared producer key active.
        let task = harness
            .run_parked(&model, harness.access(through_core).await, Some("shared"))
            .await;
        model.release.notify_one();
        let (status, body) = task.await.unwrap();
        assert_eq!(status, StatusCode::OK, "core={through_core}: {body}");
        assert_eq!(harness.active_runs(), 1);

        // The same key again: it would coalesce onto that run.
        let task = harness
            .run_parked(&model, harness.access(through_core).await, Some("shared"))
            .await;
        assert!(harness.ctx.auth.pairing().revoke_token(TOKEN));
        model.release.notify_one();
        let (status, body) = task.await.unwrap();
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "core={through_core}: a revoked credential must not coalesce: {body}"
        );
        assert_eq!(harness.active_runs(), 1, "core={through_core}");
    }
}

/// A principal whose agent selector names only `alpha` may not delete a
/// procedure that runs as `beta`, and may not rename it either.
#[tokio::test]
async fn rename_holds_the_existing_procedure_to_the_agent_selector() {
    use zeroclaw_api::grants::{Resource, Verb};
    use zeroclaw_config::schema::{PermissionProfileConfig, UserConfig};
    use zeroclaw_runtime::rpc::{
        dispatch::RpcDispatcher,
        inproc::InprocTransport,
        transport::{RpcTransport, TransportKind},
    };
    let mut foreign = gated_sop();
    foreign.name = "foreign".into();
    foreign.agent = Some("beta".into());
    let harness = Harness::with_sops(vec![foreign], approval(1, &[TOKEN]));
    {
        let mut config = harness.ctx.config.write();
        config.security.trust_daemon_uid = false;
        config.agents.insert("alpha".into(), Default::default());
        config.agents.insert("beta".into(), Default::default());
        config.permission_profiles.insert(
            "only-alpha".into(),
            PermissionProfileConfig {
                allowed_agents: vec!["alpha".into()],
                allowed_tools: vec!["*".into()],
                grants: HashMap::from([(
                    Resource::Sops,
                    vec![Verb::Read, Verb::Update, Verb::Delete],
                )]),
                ..Default::default()
            },
        );
        config.users.insert(
            "alice".into(),
            UserConfig {
                principal_id: None,
                uid: Some(4242),
                permission_profiles: vec!["only-alpha".into()],
            },
        );
        harness.ctx.auth.refresh_from_config(&config).unwrap();
    }
    let (client, server) = tokio::io::duplex(64 * 1024);
    let mut transport = InprocTransport::new(server, harness.cancel.clone());
    let mut dispatcher = RpcDispatcher::new(
        harness.ctx.clone(),
        transport.writer(),
        "local:fixture".into(),
    )
    .with_transport(
        TransportKind::Local,
        zeroclaw_runtime::security::auth_provider::Credential::Peercred { uid: 4242 },
    );
    let task = zeroclaw_spawn::spawn!(async move { dispatcher.run(&mut transport).await });
    let client = zeroclaw_rpc_client::RpcClient::connect_over(client, Default::default())
        .await
        .unwrap();
    assert_eq!(
        client.handshake().principal_id.as_deref(),
        Some("user:alice")
    );

    let forbidden = |result: &Result<Value, zeroclaw_rpc_client::ClientError>| {
        matches!(result, Err(zeroclaw_rpc_client::ClientError::Rpc(error))
            if error.code == zeroclaw_api::jsonrpc::error_codes::FORBIDDEN)
    };
    let deleted = client
        .request(
            zeroclaw_rpc_client::Method::SopsDelete,
            json!({ "name": "foreign" }),
        )
        .await;
    assert!(forbidden(&deleted), "{deleted:?}");
    let renamed = client
        .request(
            zeroclaw_rpc_client::Method::SopsRename,
            json!({ "from": "foreign", "to": "stolen" }),
        )
        .await;
    client.shutdown();
    task.abort();
    assert!(forbidden(&renamed), "{renamed:?}");
    let dir = std::path::PathBuf::from(harness.ctx.config.read().sop.sops_dir.clone().unwrap());
    assert!(dir.join("foreign").exists() && !dir.join("stolen").exists());
}

#[tokio::test]
async fn a_run_is_refused_when_its_procedure_is_deleted_during_the_decision_wait() {
    // A second run under the same dedup key would coalesce onto the first,
    // which a reload keeps active. With the definition gone there is nothing
    // to admit the caller against, so it must not join that run either.
    let mut joined = Vec::new();
    for through_core in [false, true] {
        let h = Harness::with_sops(vec![gated_sop()], approval(1, &[TOKEN]));
        let model = h.park_decisions();
        let first = h
            .run_parked(&model, h.access(through_core).await, Some("shared"))
            .await;
        model.release.notify_one();
        let (status, body) = first.await.unwrap();
        assert_eq!(status, StatusCode::OK, "first run: {body}");
        let first_id = body["run_id"].clone();
        assert!(first_id.is_string());
        assert_eq!(h.active_runs(), 1);

        let waiting = h
            .run_parked(&model, h.access(through_core).await, Some("shared"))
            .await;
        let dir = std::path::PathBuf::from(h.state.config.read().sop.sops_dir.clone().unwrap());
        zeroclaw_runtime::sop::delete_sop_typed(&dir, "deploy").unwrap();
        h.engine().lock().unwrap().reload(h._dir.path());
        assert!(h.engine().lock().unwrap().get_sop("deploy").is_none());
        assert_eq!(h.active_runs(), 1, "a reload keeps the admitted run");
        assert!(h.ctx.auth.pairing().revoke_token(TOKEN));
        model.release.notify_one();
        let (status, body) = waiting.await.unwrap();
        assert_eq!(h.active_runs(), 1);
        if status.is_success() {
            joined.push((through_core, status, body, first_id));
        }
    }
    assert!(
        joined.is_empty(),
        "a run of a deleted procedure joined the retained run without an admission \
         (core, status, body, first run): {joined:?}"
    );
}

/// How many open descriptors in this process refer to the file `metadata`
/// describes.
#[cfg(unix)]
fn open_descriptors_of(metadata: &std::fs::Metadata) -> usize {
    use std::os::unix::fs::MetadataExt;
    // libc's device and inode widths vary by Unix target. Preserve the u64
    // identity used by MetadataExt without casting an already-u64 field.
    fn stat_id(value: impl Into<i128>) -> u64 {
        value.into() as u64
    }
    std::fs::read_dir("/dev/fd")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<libc::c_int>().ok())
        .filter(|fd| {
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: fstat accepts any descriptor and reports EBADF for one
            // that closed during the listing; `stat` is allocated storage and
            // is read only after fstat succeeded.
            if unsafe { libc::fstat(*fd, stat.as_mut_ptr()) } != 0 {
                return false;
            }
            // SAFETY: fstat returned 0, so it initialized `stat`.
            let stat = unsafe { stat.assume_init() };
            stat_id(stat.st_dev) == metadata.dev() && stat_id(stat.st_ino) == metadata.ino()
        })
        .count()
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_rechecks_the_agent_selector_after_waiting_for_the_authoring_lock() {
    use zeroclaw_api::grants::{Resource, Verb};
    use zeroclaw_config::schema::{PermissionProfileConfig, UserConfig};
    use zeroclaw_runtime::rpc::{
        dispatch::RpcDispatcher,
        inproc::InprocTransport,
        transport::{RpcTransport, TransportKind},
    };
    let mut sop = gated_sop();
    sop.name = "owned".into();
    sop.agent = Some("alpha".into());
    let h = Harness::with_sops(vec![sop], approval(1, &[TOKEN]));
    {
        let mut cfg = h.ctx.config.write();
        cfg.security.trust_daemon_uid = false;
        cfg.agents.insert("alpha".into(), Default::default());
        cfg.agents.insert("beta".into(), Default::default());
        cfg.permission_profiles.insert(
            "rename-scope".into(),
            PermissionProfileConfig {
                allowed_agents: vec!["alpha".into()],
                allowed_tools: vec!["*".into()],
                grants: HashMap::from([(
                    Resource::Sops,
                    vec![Verb::Read, Verb::Update, Verb::Delete],
                )]),
                ..Default::default()
            },
        );
        cfg.users.insert(
            "rename-user".into(),
            UserConfig {
                principal_id: None,
                uid: Some(4242),
                permission_profiles: vec!["rename-scope".into()],
            },
        );
        h.ctx.auth.refresh_from_config(&cfg).unwrap();
    }
    let (client, server) = tokio::io::duplex(64 * 1024);
    let mut transport = InprocTransport::new(server, h.cancel.clone());
    let mut dispatcher =
        RpcDispatcher::new(h.ctx.clone(), transport.writer(), "local:rename".into())
            .with_transport(
                TransportKind::Local,
                zeroclaw_runtime::security::auth_provider::Credential::Peercred { uid: 4242 },
            );
    let server_task = zeroclaw_spawn::spawn!(async move { dispatcher.run(&mut transport).await });
    let client = Arc::new(
        zeroclaw_rpc_client::RpcClient::connect_over(client, Default::default())
            .await
            .unwrap(),
    );
    assert_eq!(
        client.handshake().principal_id.as_deref(),
        Some("user:rename-user")
    );

    let dir = std::path::PathBuf::from(h.ctx.config.read().sop.sops_dir.clone().unwrap());
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join(".sop-authoring.lock"))
        .unwrap();
    held.lock().unwrap();
    let metadata = held.metadata().unwrap();
    let before = open_descriptors_of(&metadata);
    assert!(before > 0);
    let requester = Arc::clone(&client);
    let pending = zeroclaw_spawn::spawn!(async move {
        requester
            .request(
                zeroclaw_rpc_client::Method::SopsRename,
                json!({"from": "owned", "to": "moved"}),
            )
            .await
    });
    // The rename opens the lock file only after its early check, so one more
    // descriptor on it means the rename passed that check and is waiting for
    // the transaction.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while open_descriptors_of(&metadata) <= before {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the rename passed its early check and is waiting for the authoring lock");
    assert!(!pending.is_finished());
    {
        let mut cfg = h.ctx.config.write();
        cfg.permission_profiles
            .get_mut("rename-scope")
            .unwrap()
            .allowed_agents = vec!["beta".into()];
        h.ctx.auth.refresh_from_config(&cfg).unwrap();
    }
    held.unlock().unwrap();
    drop(held);
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap();
    let current = if dir.join("moved").exists() {
        "moved"
    } else {
        "owned"
    };
    let control = client
        .request(
            zeroclaw_rpc_client::Method::SopsRename,
            json!({"from": current, "to": "again"}),
        )
        .await;
    assert!(
        matches!(&control, Err(zeroclaw_rpc_client::ClientError::Rpc(e)) if e.code == zeroclaw_api::jsonrpc::error_codes::FORBIDDEN),
        "a fresh request must see the narrowed selector: {control:?}"
    );
    client.shutdown();
    server_task.abort();
    let moved = dir.join("moved").exists();
    let original = dir.join("owned").exists();
    assert!(
        matches!(&result, Err(zeroclaw_rpc_client::ClientError::Rpc(e)) if e.code == zeroclaw_api::jsonrpc::error_codes::FORBIDDEN)
            && original
            && !moved,
        "the rename used the selector from before its wait: {result:?}, owned={original}, moved={moved}"
    );
}

/// An admission whose permit holds a policy read lock, the way the core's
/// authority lease does, and that queues a policy writer behind its first
/// permit.
struct QueuedWriterAdmission {
    policy: Arc<parking_lot::RwLock<()>>,
    calls: std::sync::atomic::AtomicUsize,
    writer: std::sync::Mutex<Option<std::thread::JoinHandle<bool>>>,
}

impl zeroclaw_runtime::sop::dispatch::SopRunAdmission for QueuedWriterAdmission {
    fn admit<'a>(
        &'a self,
        _sop: &Sop,
    ) -> Option<Box<dyn zeroclaw_runtime::sop::dispatch::HeldPermit + 'a>> {
        use std::sync::atomic::Ordering;
        let held = self.policy.read();
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            let policy = Arc::clone(&self.policy);
            *self.writer.lock().unwrap() = Some(std::thread::spawn(move || {
                // A real publication waits without limit; this one gives up
                // so a regression fails instead of hanging the suite.
                policy
                    .try_write_for(std::time::Duration::from_secs(2))
                    .is_some()
            }));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            while !(self.policy.is_locked_exclusive() && self.policy.try_read_recursive().is_some())
            {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the writer did not queue"
                );
                std::thread::yield_now();
            }
        }
        Some(Box::new(held))
    }
}

#[tokio::test]
async fn a_procedure_name_two_manifests_share_is_admitted_once() {
    use std::sync::atomic::Ordering;
    let h = Harness::with_sops(vec![gated_sop()], approval(1, &[TOKEN]));
    let root = std::path::PathBuf::from(h.ctx.config.read().sop.sops_dir.clone().unwrap());
    let copy = root.join("second-directory");
    std::fs::create_dir(&copy).unwrap();
    std::fs::copy(root.join("deploy/SOP.toml"), copy.join("SOP.toml")).unwrap();
    std::fs::copy(root.join("deploy/SOP.md"), copy.join("SOP.md")).unwrap();
    h.engine().lock().unwrap().reload(h._dir.path());
    let loaded = h
        .engine()
        .lock()
        .unwrap()
        .sops()
        .iter()
        .filter(|sop| sop.name == "deploy")
        .count();
    // A loader that refused or merged the second manifest would close this
    // case by itself; today it keeps both.
    if loaded < 2 {
        return;
    }
    let admission = QueuedWriterAdmission {
        policy: Arc::new(parking_lot::RwLock::new(())),
        calls: 0.into(),
        writer: Default::default(),
    };
    let results = zeroclaw_runtime::sop::dispatch::dispatch_sop_event_to_admitted(
        h.engine(),
        h.state.sop_audit.as_ref().unwrap(),
        SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: None,
            timestamp: now_iso8601(),
        },
        "deploy",
        None,
        &admission,
    )
    .await;
    assert_eq!(admission.calls.load(Ordering::SeqCst), 1, "{results:?}");
    let wrote = admission
        .writer
        .lock()
        .unwrap()
        .take()
        .unwrap()
        .join()
        .unwrap();
    assert!(
        wrote,
        "a second admission on the dispatching thread waited behind the queued writer"
    );
}
