//! Parity of the routes served through the core with their in-process bodies.
//!
//! Each test runs a route's handler twice over one shared state, once
//! in-process and once through the daemon's real in-process connector, and
//! requires the same status and body. The state the two paths read (cost
//! tracker, TUI registry, event history, pairing) is the same instance, as
//! it is under the daemon.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use zeroclaw_config::cost::{CostTracker, TokenUsage};
use zeroclaw_rpc_proto::types::CLIENT_KIND_GATEWAY;
use zeroclaw_runtime::rpc::context::RpcContext;
use zeroclaw_runtime::rpc::inproc::InprocConnector;
use zeroclaw_runtime::rpc::tui_identity::TuiEntry;

use crate::AppState;
use crate::api::{
    CostQuery, CronRunsQuery, MemoryDeleteQuery, MemoryQuery, MemoryStoreBody, handle_api_cost,
    handle_api_cron_delete, handle_api_cron_list, handle_api_cron_run, handle_api_cron_runs,
    handle_api_cron_settings_get, handle_api_cron_settings_patch, handle_api_health,
    handle_api_memory_delete, handle_api_memory_list, handle_api_memory_store, handle_api_tuis,
};
use crate::core_rpc::{CoreAccess, CoreRpc, DedicatedCoreAccess};
use crate::sse::{EventBuffer, handle_events_history};

const TOKEN: &str = "zc_parity_operator";
const AGENT: &str = "parity-agent";

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

/// One memory store the gateway state and the core both hold, as under the
/// daemon.
type SharedMemory = Arc<dyn zeroclaw_api::memory_traits::Memory>;

impl Harness {
    fn new(cost_tracker: Option<Arc<CostTracker>>) -> Self {
        Self::build(cost_tracker, None)
    }

    /// A harness with a configured agent (for cron jobs) whose gateway state
    /// and core share `memory`.
    fn with_memory(memory: SharedMemory) -> Self {
        Self::build(None, Some(memory))
    }

    fn build(cost_tracker: Option<Arc<CostTracker>>, memory: Option<SharedMemory>) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = zeroclaw_config::schema::Config {
            data_dir: dir.path().to_path_buf(),
            config_path: dir.path().join("config.toml"),
            ..Default::default()
        };
        config.gateway.require_pairing = true;
        config.gateway.paired_tokens = vec![TOKEN.into()];
        config.providers.models.openrouter.insert(
            "default".to_string(),
            zeroclaw_config::schema::OpenRouterModelProviderConfig::default(),
        );
        config.risk_profiles.insert(
            "parity-profile".to_string(),
            zeroclaw_config::schema::RiskProfileConfig::default(),
        );
        config.agents.insert(
            AGENT.to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "openrouter.default".into(),
                risk_profile: "parity-profile".into(),
                ..Default::default()
            },
        );
        // The daemon's TUI identity signing key. The in-process connector is
        // a non-local caller, and the core refuses its `initialize` while
        // signing is off.
        std::fs::write(dir.path().join(".secret_key"), "42".repeat(32)).expect("signing key");
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
            ctx.memory = memory.clone();
        }
        let cancel = tokio_util::sync::CancellationToken::new();
        let connector = InprocConnector::new(cancel.clone());
        connector.bind(Arc::clone(&ctx));

        let mut state = crate::api::test_state(config);
        state.pairing = Arc::clone(ctx.auth.pairing());
        state.cost_tracker = cost_tracker;
        state.event_buffer = history;
        state.tui_registry = Some(Arc::clone(&ctx.tui_registry));
        if let Some(memory) = memory {
            state.mem = memory;
        }

        Self {
            state,
            ctx,
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

    /// A request's access through the core, bound to the operator's bearer.
    async fn through_core(&self) -> CoreAccess {
        match self.core.access(&Self::headers()).await {
            Ok(access @ CoreAccess::Core(_)) => access,
            Ok(CoreAccess::InProcess) => panic!("served in-process"),
            Err(error) => panic!("no core access: {error:?}"),
        }
    }

    /// The same, on a connection of the request's own, as a manual cron run
    /// takes it.
    async fn through_core_dedicated(&self) -> DedicatedCoreAccess {
        match self.core.access_dedicated(&Self::headers()).await {
            Ok(access @ CoreAccess::Core(_)) => DedicatedCoreAccess(access),
            Ok(CoreAccess::InProcess) => panic!("served in-process"),
            Err(error) => panic!("no core access: {error:?}"),
        }
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

// ── Cron ─────────────────────────────────────────────────────────

impl Harness {
    fn config(&self) -> zeroclaw_config::schema::Config {
        self.state.config.read().clone()
    }

    fn add_job(&self, name: &str) -> zeroclaw_runtime::cron::CronJob {
        zeroclaw_runtime::cron::add_shell_job(
            &self.config(),
            AGENT,
            Some(name.to_string()),
            zeroclaw_runtime::cron::Schedule::Cron {
                expr: "*/5 * * * *".to_string(),
                tz: None,
            },
            "echo parity",
        )
        .expect("add a job")
    }

    /// A job whose row is gone while its run history is kept, as a completed
    /// one-shot leaves it.
    fn history_only_job(&self, name: &str) -> String {
        let config = self.config();
        let job = self.add_job(name);
        let started = chrono::Utc::now();
        zeroclaw_runtime::cron::record_run(
            &config,
            &job.id,
            started,
            started + chrono::Duration::milliseconds(5),
            "ok",
            zeroclaw_runtime::cron::RunOutcomes {
                execution: "ok",
                delivery: "not_required",
                persistence: "not_bound",
            },
            zeroclaw_runtime::cron::RunProvenance {
                principal: None,
                executing_agent: Some(AGENT),
                job_source: None,
            },
            Some("ok"),
            5,
        )
        .expect("record a run");
        let db = rusqlite::Connection::open(config.data_dir.join("cron").join("jobs.db"))
            .expect("open the cron store");
        db.execute("DELETE FROM cron_jobs WHERE id = ?1", [&job.id])
            .expect("drop the job row");
        assert!(zeroclaw_runtime::cron::get_job(&config, &job.id).is_err());
        assert_eq!(
            zeroclaw_runtime::cron::list_runs(&config, &job.id, 10)
                .expect("runs")
                .len(),
            1
        );
        job.id
    }
}

/// A manual run's fields that depend on when it ran.
fn without_timing((status, mut body): (StatusCode, Value)) -> (StatusCode, Value) {
    if let Some(run) = body.as_object_mut() {
        for field in ["duration_ms", "started_at", "finished_at"] {
            assert!(run.remove(field).is_some(), "{field} missing: {run:?}");
        }
    }
    (status, body)
}

#[tokio::test]
async fn cron_reads_through_the_core_match_the_in_process_bodies() {
    let harness = Harness::new(None);
    harness.add_job("parity-a");
    harness.add_job("parity-b");

    let in_process = body_of(
        handle_api_cron_list(
            State(harness.state.clone()),
            Harness::headers(),
            CoreAccess::InProcess,
        )
        .await
        .into_response(),
    )
    .await;
    let core = body_of(
        handle_api_cron_list(
            State(harness.state.clone()),
            Harness::headers(),
            harness.through_core().await,
        )
        .await
        .into_response(),
    )
    .await;
    let body = assert_same(in_process, core);
    assert_eq!(body["jobs"].as_array().expect("jobs").len(), 2, "{body}");

    let in_process = body_of(
        handle_api_cron_settings_get(
            State(harness.state.clone()),
            Harness::headers(),
            CoreAccess::InProcess,
        )
        .await
        .into_response(),
    )
    .await;
    let core = body_of(
        handle_api_cron_settings_get(
            State(harness.state.clone()),
            Harness::headers(),
            harness.through_core().await,
        )
        .await
        .into_response(),
    )
    .await;
    let body = assert_same(in_process, core);
    assert_eq!(
        body.as_object().map(|settings| settings.len()),
        Some(3),
        "{body}"
    );
}

#[tokio::test]
async fn cron_settings_patch_through_the_core_matches_the_in_process_body() {
    // Each path patches its own copy of the same starting config: the
    // gateway's state and the core's live config.
    for patch in [
        json!({}),
        json!({
            "enabled": false,
            "catch_up_on_startup": false,
            "max_run_history": 9_999_999_999_u64,
        }),
        json!({ "enabled": "no", "max_run_history": 7, "unknown": true }),
    ] {
        let harness = Harness::new(None);
        let in_process = body_of(
            handle_api_cron_settings_patch(
                State(harness.state.clone()),
                Harness::headers(),
                CoreAccess::InProcess,
                Json(patch.clone()),
            )
            .await
            .into_response(),
        )
        .await;
        let core = body_of(
            handle_api_cron_settings_patch(
                State(harness.state.clone()),
                Harness::headers(),
                harness.through_core().await,
                Json(patch.clone()),
            )
            .await
            .into_response(),
        )
        .await;
        let body = assert_same(in_process, core);
        assert_eq!(body["status"], "ok", "{patch}: {body}");
        let live = harness.ctx.config.read().scheduler.clone();
        assert_eq!(body["max_run_history"], live.max_run_history, "{patch}");
        assert_eq!(body["enabled"], live.enabled, "{patch}");
    }
}

#[tokio::test]
async fn cron_run_through_the_core_matches_the_in_process_body() {
    let harness = Harness::new(None);
    let job = harness.add_job("parity-run");

    let in_process = without_timing(
        body_of(
            handle_api_cron_run(
                State(harness.state.clone()),
                Harness::headers(),
                Path(job.id.clone()),
                DedicatedCoreAccess(CoreAccess::InProcess),
            )
            .await
            .into_response(),
        )
        .await,
    );
    let core = without_timing(
        body_of(
            handle_api_cron_run(
                State(harness.state.clone()),
                Harness::headers(),
                Path(job.id.clone()),
                harness.through_core_dedicated().await,
            )
            .await
            .into_response(),
        )
        .await,
    );
    let body = assert_same(in_process, core);
    assert_eq!(body["job_id"], job.id.as_str(), "{body}");
    assert_eq!(body["success"], true, "{body}");
}

#[tokio::test]
async fn cron_delete_through_the_core_matches_the_in_process_body() {
    let harness = Harness::new(None);
    let delete = |id: String, access| {
        let state = harness.state.clone();
        async move {
            body_of(
                handle_api_cron_delete(State(state), Harness::headers(), Path(id), access)
                    .await
                    .into_response(),
            )
            .await
        }
    };

    // A job and its history.
    let first = harness.add_job("parity-delete-a").id;
    let second = harness.add_job("parity-delete-b").id;
    let in_process = delete(first.clone(), CoreAccess::InProcess).await;
    let core = delete(second.clone(), harness.through_core().await).await;
    assert_same(in_process, core);

    // Only the retained history of a job whose row is gone.
    let first = harness.history_only_job("parity-history-a");
    let second = harness.history_only_job("parity-history-b");
    let in_process = delete(first.clone(), CoreAccess::InProcess).await;
    let core = delete(second.clone(), harness.through_core().await).await;
    assert_same(in_process, core);
    let config = harness.config();
    for id in [&first, &second] {
        assert!(
            zeroclaw_runtime::cron::list_runs(&config, id, 10)
                .expect("runs")
                .is_empty(),
            "{id}: history removed"
        );
    }

    // Neither a job nor history: the route answers a failed removal, and the
    // core path answers the core's not-found refusal the same way.
    let in_process = delete("no-such-job".to_string(), CoreAccess::InProcess).await;
    let core = delete("no-such-job".to_string(), harness.through_core().await).await;
    assert_eq!(
        in_process.0,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        in_process.1
    );
    assert_eq!(core, in_process, "an id with nothing to remove fails alike");
}

/// A run, or the runs, of a job that does not exist: refused as not found by
/// the in-process route, by the in-process route's core path, and by both
/// routers' rendering of the core's refusal, with the same body.
#[tokio::test]
async fn an_unknown_job_is_not_found_alike() {
    use crate::refusal_parity::{assert_refusal_parity, core_refusal};
    use zeroclaw_rpc_client::Method;
    let harness = Harness::new(None);
    let id = "no-such-job";
    let CoreAccess::Core(call) = harness.through_core().await else {
        panic!("served in-process");
    };

    let in_process = handle_api_cron_run(
        State(harness.state.clone()),
        Harness::headers(),
        Path(id.to_string()),
        DedicatedCoreAccess(CoreAccess::InProcess),
    )
    .await
    .into_response();
    let route = handle_api_cron_run(
        State(harness.state.clone()),
        Harness::headers(),
        Path(id.to_string()),
        harness.through_core_dedicated().await,
    )
    .await
    .into_response();
    let refused = core_refusal(&call, Method::CronTrigger, json!({ "id": id })).await;
    assert_refusal_parity(
        "POST /api/cron/{id}/run",
        StatusCode::NOT_FOUND,
        in_process,
        refused.into_iter().chain([route]),
    )
    .await;

    let runs = |access| {
        handle_api_cron_runs(
            State(harness.state.clone()),
            Harness::headers(),
            Path(id.to_string()),
            Query(CronRunsQuery { limit: None }),
            access,
        )
    };
    let in_process = runs(CoreAccess::InProcess).await.into_response();
    let route = runs(harness.through_core().await).await.into_response();
    let refused = core_refusal(&call, Method::CronRuns, json!({ "id": id, "limit": 20 })).await;
    assert_refusal_parity(
        "GET /api/cron/{id}/runs",
        StatusCode::NOT_FOUND,
        in_process,
        refused.into_iter().chain([route]),
    )
    .await;
}

#[tokio::test]
async fn cron_runs_through_the_core_match_the_in_process_body() {
    let harness = Harness::new(None);
    // More runs than the route lists at most (100), kept past the default
    // history cap, so the upper clamp shows.
    let mut config = harness.config();
    config.scheduler.max_run_history = 200;
    let job = harness.add_job("parity-runs");
    let started = chrono::Utc::now();
    for index in 0..101_i64 {
        zeroclaw_runtime::cron::record_run(
            &config,
            &job.id,
            started + chrono::Duration::seconds(index),
            started + chrono::Duration::seconds(index) + chrono::Duration::milliseconds(5),
            "ok",
            zeroclaw_runtime::cron::RunOutcomes {
                execution: "ok",
                delivery: "not_required",
                persistence: "not_bound",
            },
            zeroclaw_runtime::cron::RunProvenance {
                principal: None,
                executing_agent: Some(AGENT),
                job_source: None,
            },
            Some("ok"),
            5,
        )
        .expect("record a run");
    }
    let retained = harness.history_only_job("parity-runs-retained");
    let runs = |id: String, limit: Option<u32>, access| {
        let state = harness.state.clone();
        async move {
            body_of(
                handle_api_cron_runs(
                    State(state),
                    Harness::headers(),
                    Path(id),
                    Query(CronRunsQuery { limit }),
                    access,
                )
                .await
                .into_response(),
            )
            .await
        }
    };
    // The default, the clamp at both ends, and a job whose row is gone.
    for (id, limit, listed) in [
        (job.id.clone(), None, 20),
        (job.id.clone(), Some(0), 1),
        (job.id.clone(), Some(2), 2),
        (job.id.clone(), Some(500), 100),
        (retained.clone(), None, 1),
    ] {
        let in_process = runs(id.clone(), limit, CoreAccess::InProcess).await;
        let core = runs(id.clone(), limit, harness.through_core().await).await;
        let body = assert_same(in_process, core);
        assert_eq!(
            body["runs"].as_array().expect("runs").len(),
            listed,
            "{id} {limit:?}: {body}"
        );
    }
}

// ── Memory ───────────────────────────────────────────────────────

fn sqlite_memory() -> (SharedMemory, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let memory = zeroclaw_memory::sqlite::SqliteMemory::new("parity", dir.path())
        .expect("open a memory store");
    (Arc::new(memory), dir)
}

fn memory_query(query: Option<&str>, category: Option<&str>, agent: Option<&str>) -> MemoryQuery {
    MemoryQuery {
        query: query.map(str::to_string),
        category: category.map(str::to_string),
        since: None,
        until: None,
        agent: agent.map(str::to_string),
    }
}

impl Harness {
    async fn memory_list(&self, query: MemoryQuery, access: CoreAccess) -> (StatusCode, Value) {
        body_of(
            handle_api_memory_list(
                State(self.state.clone()),
                Self::headers(),
                Query(query),
                access,
            )
            .await
            .into_response(),
        )
        .await
    }

    async fn memory_store(&self, body: Value, access: CoreAccess) -> (StatusCode, Value) {
        let body: MemoryStoreBody = serde_json::from_value(body).expect("store body");
        body_of(
            handle_api_memory_store(
                State(self.state.clone()),
                Self::headers(),
                access,
                Json(body),
            )
            .await
            .into_response(),
        )
        .await
    }

    async fn memory_delete(
        &self,
        key: &str,
        agent: Option<&str>,
        access: CoreAccess,
    ) -> (StatusCode, Value) {
        body_of(
            handle_api_memory_delete(
                State(self.state.clone()),
                Self::headers(),
                Path(key.to_string()),
                Query(MemoryDeleteQuery {
                    agent: agent.map(str::to_string),
                }),
                access,
            )
            .await
            .into_response(),
        )
        .await
    }
}

#[tokio::test]
async fn memory_routes_through_the_core_match_the_in_process_bodies() {
    let (memory, _memory_dir) = sqlite_memory();
    let harness = Harness::with_memory(memory);

    // Stores without a category file under `core` either way.
    let in_process = harness
        .memory_store(
            json!({ "key": "parity-a", "content": "alpha parity note" }),
            CoreAccess::InProcess,
        )
        .await;
    let core = harness
        .memory_store(
            json!({ "key": "parity-b", "content": "beta parity note" }),
            harness.through_core().await,
        )
        .await;
    assert_same(in_process, core);
    let long = "x".repeat(5_000);
    for (key, content, category) in [
        ("parity-daily", "gamma parity note", "daily"),
        ("parity-long", long.as_str(), "core"),
    ] {
        let in_process = harness
            .memory_store(
                json!({ "key": key, "content": content, "category": category }),
                CoreAccess::InProcess,
            )
            .await;
        assert_eq!(in_process.0, StatusCode::OK, "{}", in_process.1);
    }

    // Listing, filtered listing, search, filtered search.
    for query in [
        memory_query(None, None, None),
        memory_query(None, Some("core"), None),
        memory_query(None, Some("daily"), None),
        memory_query(Some("parity"), None, None),
        memory_query(Some("parity"), Some("daily"), None),
    ] {
        let described = format!("{:?}/{:?}", query.query, query.category);
        let in_process = harness
            .memory_list(
                memory_query(
                    query.query.as_deref(),
                    query.category.as_deref(),
                    query.agent.as_deref(),
                ),
                CoreAccess::InProcess,
            )
            .await;
        let core = harness
            .memory_list(query, harness.through_core().await)
            .await;
        let body = assert_same(in_process, core);
        assert!(
            !body["entries"].as_array().expect("entries").is_empty(),
            "{described}: {body}"
        );
    }
    let (_, body) = harness
        .memory_list(memory_query(None, None, None), harness.through_core().await)
        .await;
    let long_entry = body["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| entry["key"] == "parity-long")
        .expect("the long entry");
    let content = long_entry["content"].as_str().expect("content");
    assert_eq!(content.chars().count(), 4096);
    assert!(content.ends_with("..."));
    // Stored without a category, in-process (`parity-a`) or through the core
    // (`parity-b`), an entry files under `core` either way.
    for key in ["parity-a", "parity-b"] {
        let entry = body["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .find(|entry| entry["key"] == key)
            .unwrap_or_else(|| panic!("{key} stored: {body}"));
        assert_eq!(entry["category"], "core", "{key}: {body}");
    }

    // An agent that does not exist: refused as a bad request either way, with
    // the same body.
    let in_process = harness
        .memory_list(
            memory_query(None, None, Some("no-such-agent")),
            CoreAccess::InProcess,
        )
        .await;
    let core = harness
        .memory_list(
            memory_query(None, None, Some("no-such-agent")),
            harness.through_core().await,
        )
        .await;
    assert_eq!(in_process.0, StatusCode::BAD_REQUEST, "{}", in_process.1);
    assert_eq!(core, in_process, "an unknown agent is refused alike");

    // Deleting reports whether an entry was there.
    let in_process = harness
        .memory_delete("parity-a", None, CoreAccess::InProcess)
        .await;
    let core = harness
        .memory_delete("parity-b", None, harness.through_core().await)
        .await;
    let body = assert_same(in_process, core);
    assert_eq!(body["deleted"], true);
    // A key with no entry is no failure: `deleted: false` either way.
    let in_process = harness
        .memory_delete("parity-a", None, CoreAccess::InProcess)
        .await;
    let core = harness
        .memory_delete("parity-b", None, harness.through_core().await)
        .await;
    let body = assert_same(in_process, core);
    assert_eq!(body["deleted"], false);
}

/// A memory store whose every call fails, as a broken backend does.
struct FailingMemory;

const STORE_DOWN: &str = "the store is down";

#[async_trait::async_trait]
impl zeroclaw_memory::Memory for FailingMemory {
    fn name(&self) -> &str {
        "failing"
    }

    async fn store(
        &self,
        _key: &str,
        _content: &str,
        _category: zeroclaw_memory::MemoryCategory,
        _session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        anyhow::bail!(STORE_DOWN)
    }

    async fn recall(
        &self,
        _query: &str,
        _limit: usize,
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
        anyhow::bail!(STORE_DOWN)
    }

    async fn get(&self, _key: &str) -> anyhow::Result<Option<zeroclaw_memory::MemoryEntry>> {
        anyhow::bail!(STORE_DOWN)
    }

    async fn list(
        &self,
        _category: Option<&zeroclaw_memory::MemoryCategory>,
        _session_id: Option<&str>,
    ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
        anyhow::bail!(STORE_DOWN)
    }

    async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
        anyhow::bail!(STORE_DOWN)
    }

    async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
        anyhow::bail!(STORE_DOWN)
    }

    async fn count(&self) -> anyhow::Result<usize> {
        anyhow::bail!(STORE_DOWN)
    }

    async fn health_check(&self) -> bool {
        false
    }

    async fn store_with_agent(
        &self,
        _key: &str,
        _content: &str,
        _category: zeroclaw_memory::MemoryCategory,
        _session_id: Option<&str>,
        _namespace: Option<&str>,
        _importance: Option<f64>,
        _agent_id: Option<&str>,
    ) -> anyhow::Result<()> {
        anyhow::bail!(STORE_DOWN)
    }

    async fn recall_for_agents(
        &self,
        _allowed_agent_ids: &[&str],
        _query: &str,
        _limit: usize,
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
        anyhow::bail!(STORE_DOWN)
    }
}

impl zeroclaw_api::attribution::Attributable for FailingMemory {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Memory(zeroclaw_api::attribution::MemoryKind::InMemory)
    }

    fn alias(&self) -> &str {
        "failing"
    }
}

impl Harness {
    /// The in-process route, or the route through the core.
    async fn access(&self, through_core: bool) -> CoreAccess {
        if through_core {
            self.through_core().await
        } else {
            CoreAccess::InProcess
        }
    }
}

/// A store that fails answers `500` on both paths, with the store's own
/// error in each body. Only the wording around it can differ: an internal
/// failure names no refusal reason, so the core path's body is the core's
/// message and error code.
#[tokio::test]
async fn a_failing_store_answers_500_on_both_paths() {
    let harness = Harness::with_memory(Arc::new(FailingMemory));
    let fails_with = |what: &str, (status, body): (StatusCode, Value), cause: &str| {
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{what}: {body}");
        let error = body["error"].as_str().unwrap_or_default();
        assert!(error.contains(cause), "{what}: {body}");
    };

    for through_core in [false, true] {
        let path = if through_core { "core" } else { "in-process" };
        for query in [None, Some("parity")] {
            let answer = harness
                .memory_list(
                    memory_query(query, None, None),
                    harness.access(through_core).await,
                )
                .await;
            fails_with(
                &format!("{path}: GET /api/memory {query:?}"),
                answer,
                STORE_DOWN,
            );
        }
        let answer = harness
            .memory_store(
                json!({ "key": "parity", "content": "parity" }),
                harness.access(through_core).await,
            )
            .await;
        fails_with(&format!("{path}: POST /api/memory"), answer, STORE_DOWN);
        let answer = harness
            .memory_delete("parity", None, harness.access(through_core).await)
            .await;
        fails_with(
            &format!("{path}: DELETE /api/memory/parity"),
            answer,
            STORE_DOWN,
        );
    }

    // A cron store that is not a database.
    let config = harness.config();
    let cron_dir = config.data_dir.join("cron");
    std::fs::create_dir_all(&cron_dir).expect("a cron directory");
    std::fs::write(cron_dir.join("jobs.db"), "not a database").expect("a broken cron store");
    let cause = zeroclaw_runtime::cron::list_jobs(&config)
        .expect_err("the broken store fails")
        .to_string();
    for through_core in [false, true] {
        let path = if through_core { "core" } else { "in-process" };
        let answer = body_of(
            handle_api_cron_list(
                State(harness.state.clone()),
                Harness::headers(),
                harness.access(through_core).await,
            )
            .await
            .into_response(),
        )
        .await;
        fails_with(&format!("{path}: GET /api/cron"), answer, &cause);
    }
}
