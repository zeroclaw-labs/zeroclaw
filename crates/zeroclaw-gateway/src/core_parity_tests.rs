//! Parity of the routes served through the core with their in-process bodies.
//!
//! Each test runs a route's handler twice over one shared state, once
//! in-process and once through the daemon's real in-process connector, and
//! requires the same status and body. The state the two paths read (cost
//! tracker, TUI registry, event history, pairing) is the same instance, as
//! it is under the daemon.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
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
use crate::api::{CostQuery, handle_api_cost, handle_api_health, handle_api_tuis};
use crate::core_rpc::{CoreAccess, CoreRpc};
use crate::sse::{EventBuffer, handle_events_history};

const TOKEN: &str = "zc_parity_operator";

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

impl Harness {
    fn new(cost_tracker: Option<Arc<CostTracker>>) -> Self {
        Self::configured(cost_tracker, |_| {})
    }

    fn configured(
        cost_tracker: Option<Arc<CostTracker>>,
        configure: impl FnOnce(&mut zeroclaw_config::schema::Config),
    ) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = zeroclaw_config::schema::Config {
            data_dir: dir.path().to_path_buf(),
            config_path: dir.path().join("config.toml"),
            ..Default::default()
        };
        config.gateway.require_pairing = true;
        config.gateway.paired_tokens = vec![TOKEN.into()];
        configure(&mut config);
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
        let mut state = crate::api::test_state(config.clone());
        // As under the daemon: the gateway's install-wide model is the
        // daemon's seed entry, and both read one memory backend and one bus.
        let (model, temperature) = zeroclaw_runtime::status::install_wide_model(&config);
        state.model = model;
        state.temperature = temperature;
        let mut ctx = RpcContext::for_live_test(config.clone(), sessions);
        assert!(ctx.tui_registry.signing_is_enabled());
        {
            let ctx = Arc::get_mut(&mut ctx).expect("a fresh context is unshared");
            ctx.cost_tracker = cost_tracker.clone();
            ctx.event_history = Some(Arc::clone(&history));
            ctx.memory = Some(Arc::clone(&state.mem));
            ctx.event_tx = Some(state.event_tx.clone());
        }
        let cancel = tokio_util::sync::CancellationToken::new();
        let connector = InprocConnector::new(cancel.clone());
        connector.bind(Arc::clone(&ctx));

        state.pairing = Arc::clone(ctx.auth.pairing());
        state.cost_tracker = cost_tracker;
        state.event_buffer = history;
        state.tui_registry = Some(Arc::clone(&ctx.tui_registry));

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

// ── Status, doctor, logs and the event stream ────────────────────

/// A provider entry with a model and temperature, and an agent on it.
fn with_a_provider_and_an_agent(config: &mut zeroclaw_config::schema::Config) {
    let entry = config
        .providers
        .models
        .ensure("openai", "parity")
        .expect("`openai` slot must exist");
    entry.model = Some("parity-model".into());
    entry.temperature = Some(0.4);
    config.agents.insert(
        "parity-agent".into(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: true,
            model_provider: "openai.parity".into(),
            ..Default::default()
        },
    );
}

/// The status body without what moves between two reads of one process:
/// the resource sample and the snapshot's own timestamp.
fn steady_status((status, mut body): (StatusCode, Value)) -> (StatusCode, Value) {
    if let Some(fields) = body.as_object_mut() {
        fields.remove("process");
        if let Some(health) = fields.get_mut("health").and_then(Value::as_object_mut) {
            health.remove("updated_at");
        }
    }
    (status, body)
}

#[tokio::test]
async fn api_status_through_the_core_matches_the_in_process_body() {
    // The second configuration names a runtime shell the core cannot
    // describe; the route has always answered regardless.
    for broken_runtime in [false, true] {
        let harness = Harness::configured(None, |config| {
            with_a_provider_and_an_agent(config);
            if broken_runtime {
                config.runtime.shell = Some("   ".into());
            }
        });
        status_parity(&harness).await;
    }
}

async fn status_parity(harness: &Harness) {
    for agent in [None, Some("parity-agent"), Some("no-such-agent"), Some(" ")] {
        let query = || crate::api::StatusQuery {
            agent: agent.map(String::from),
        };
        // Uptime and the process-wide component table can move between the
        // reads: compare against an in-process body that held still.
        for attempt in 0.. {
            let read_in_process = || async {
                steady_status(
                    body_of(
                        crate::api::handle_api_status(
                            State(harness.state.clone()),
                            Harness::headers(),
                            Query(query()),
                            CoreAccess::InProcess,
                        )
                        .await
                        .into_response(),
                    )
                    .await,
                )
            };
            let before = read_in_process().await;
            let core = steady_status(
                body_of(
                    crate::api::handle_api_status(
                        State(harness.state.clone()),
                        Harness::headers(),
                        Query(query()),
                        harness.through_core().await,
                    )
                    .await
                    .into_response(),
                )
                .await,
            );
            let after = read_in_process().await;
            if before != after {
                assert!(attempt < 20, "the status snapshot never held still");
                continue;
            }
            let body = assert_same(before, core);
            match agent {
                Some("parity-agent") => {
                    assert_eq!(body["model"], "parity-model");
                    assert_eq!(body["model_provider"], "openai.parity");
                }
                _ => {
                    assert_eq!(body["model"], "parity-model");
                    assert_eq!(body["temperature"], 0.4);
                }
            }
            break;
        }
    }
}

/// The doctor body without the one figure that moves between two runs: the
/// free disk space other processes are writing to.
fn steady_doctor((status, mut body): (StatusCode, Value)) -> (StatusCode, Value) {
    for result in body["results"].as_array_mut().into_iter().flatten() {
        if result["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("disk space: "))
        {
            result["message"] = json!("disk space: <free> MB available");
        }
    }
    (status, body)
}

#[tokio::test]
async fn api_doctor_through_the_core_matches_the_in_process_body() {
    let harness = Harness::configured(None, with_a_provider_and_an_agent);
    let in_process = steady_doctor(
        body_of(
            crate::api::handle_api_doctor(
                State(harness.state.clone()),
                Harness::headers(),
                CoreAccess::InProcess,
            )
            .await
            .into_response(),
        )
        .await,
    );
    let core = steady_doctor(
        body_of(
            crate::api::handle_api_doctor(
                State(harness.state.clone()),
                Harness::headers(),
                harness.through_core().await,
            )
            .await
            .into_response(),
        )
        .await,
    );
    let body = assert_same(in_process, core);
    assert!(
        body["results"].as_array().is_some_and(|r| !r.is_empty()),
        "{body}"
    );
}

async fn logs_body(
    harness: &Harness,
    query: &[(&str, &str)],
    through_core: bool,
) -> (StatusCode, Value) {
    let params: HashMap<String, String> = query
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    let access = if through_core {
        harness.through_core().await
    } else {
        CoreAccess::InProcess
    };
    body_of(
        crate::api_logs::handle_api_logs(
            State(harness.state.clone()),
            Harness::headers(),
            Query(params),
            access,
        )
        .await,
    )
    .await
}

fn install_log_writer(dir: &std::path::Path, persistence: &str) {
    zeroclaw_log::init_from_config(
        &zeroclaw_log::LogConfig {
            log_persistence: persistence.into(),
            log_persistence_path: "state/runtime-trace.jsonl".into(),
            ..Default::default()
        },
        dir,
    );
}

#[tokio::test]
async fn api_logs_through_the_core_matches_the_in_process_body() {
    let _writer = zeroclaw_log::__private_test_writer_lock();
    let harness = Harness::new(None);
    let logs = tempfile::tempdir().expect("tempdir");

    // Persistence off: both answer the empty page that says so.
    install_log_writer(logs.path(), "none");
    let in_process = logs_body(&harness, &[], false).await;
    let body = assert_same(in_process, logs_body(&harness, &[], true).await);
    assert_eq!(body["persistence_enabled"], false, "{body}");

    // Persistence on, with events other tests in this process may add to:
    // every query below is narrowed to this test's own marker.
    install_log_writer(logs.path(), "rolling");
    let marker = uuid::Uuid::new_v4().to_string();
    for (agent, channel) in [
        ("alpha", "telegram.ops"),
        ("beta", "telegram.ops"),
        ("alpha", "discord.dev"),
    ] {
        let mut event = zeroclaw_log::LogEvent::new(
            zeroclaw_log::Severity::Info,
            "note",
            zeroclaw_log::EventCategory::Agent,
        );
        event.message = Some(marker.clone());
        // The logging layer writes a composite field with its decomposed
        // keys; a raw event carries them only when set here.
        let (channel_type, channel_alias) = channel.split_once('.').expect("<type>.<alias>");
        for (key, value) in [
            ("agent_alias", agent),
            ("channel", channel),
            ("channel_type", channel_type),
            ("channel_alias", channel_alias),
        ] {
            event.zeroclaw.fields.insert(key.into(), value.into());
        }
        zeroclaw_log::record_event(event);
    }
    zeroclaw_log::flush_for_test().expect("flush");

    for (query, expected) in [
        (vec![("q", marker.as_str())], Some(3)),
        (
            vec![("q", marker.as_str()), ("agent_alias", "alpha")],
            Some(2),
        ),
        (
            vec![
                ("q", marker.as_str()),
                ("agent_alias", "alpha"),
                ("channel", "telegram.ops"),
            ],
            Some(1),
        ),
        (
            vec![("q", marker.as_str()), ("channel_alias", "dev")],
            Some(1),
        ),
        (vec![("q", marker.as_str()), ("limit", "1")], Some(1)),
        (
            vec![("q", marker.as_str()), ("limit", "not-a-number")],
            Some(3),
        ),
        (vec![("q", marker.as_str()), ("agent_alias", "")], Some(3)),
        (vec![("q", marker.as_str()), ("no_such_field", "x")], None),
        (vec![("until_segment_cursor", "garbage")], None),
    ] {
        let in_process = logs_body(&harness, &query, false).await;
        let core = logs_body(&harness, &query, true).await;
        assert_eq!(core, in_process, "{query:?}");
        match expected {
            Some(count) => {
                assert_eq!(in_process.0, StatusCode::OK, "{query:?}: {}", in_process.1);
                assert_eq!(
                    in_process.1["events"].as_array().map(Vec::len),
                    Some(count),
                    "{query:?}: {}",
                    in_process.1
                );
                assert_eq!(in_process.1["persistence_enabled"], true);
            }
            None => assert_eq!(in_process.0, StatusCode::BAD_REQUEST, "{query:?}"),
        }
    }

    install_log_writer(logs.path(), "none");
}

/// Read `count` events from an SSE body, or what arrived before `wait`.
async fn sse_events(response: Response, count: usize) -> Vec<Value> {
    let mut body = response.into_body();
    let mut text = String::new();
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while events.len() < count {
        let Ok(Some(Ok(frame))) = tokio::time::timeout_at(deadline, body.frame()).await else {
            break;
        };
        if let Ok(data) = frame.into_data() {
            text.push_str(&String::from_utf8_lossy(&data));
        }
        while let Some(end) = text.find("\n\n") {
            let block: String = text.drain(..end + 2).collect();
            for line in block.lines() {
                if let Some(data) = line.strip_prefix("data: ") {
                    events.push(serde_json::from_str(data).expect("an SSE data line is JSON"));
                }
            }
        }
    }
    events
}

#[tokio::test]
async fn the_event_stream_through_the_core_carries_the_in_process_frames_but_no_credentials() {
    let harness = Harness::new(None);
    let in_process =
        crate::sse::handle_sse_events(State(harness.state.clone()), Harness::headers())
            .await
            .into_response();
    let CoreAccess::Core(call) = harness.through_core().await else {
        unreachable!("through_core answers Core");
    };
    let core = crate::sse::events_stream_through_core(call)
        .await
        .expect("the core opens the stream");

    let frames = [
        json!({"type": "agent_start", "source": "observability", "provider": "test"}),
        json!({"type": "message", "session_id": "s-1", "content": "private"}),
        json!({"type": "channel_login", "channel": "whatsapp.ops", "status": "waiting"}),
        {
            let mut frame = json!({
                "type": "channel_login",
                "channel": "whatsapp.ops",
                "qr": "pairing-secret",
            });
            frame[zeroclaw_log::EPHEMERAL_BROADCAST_MARKER] = json!(true);
            frame
        },
        json!({"type": "tool_call", "source": "observability", "tool": "shell"}),
    ];
    for frame in &frames {
        harness
            .state
            .event_tx
            .send(frame.clone())
            .expect("subscribers");
    }

    let from_process = sse_events(in_process, 4).await;
    let from_core = sse_events(core, 3).await;
    // The in-process stream: every public frame, and the pairing frame for
    // its authenticated subscriber, marker stripped.
    assert_eq!(from_process.len(), 4, "{from_process:?}");
    assert!(from_process.iter().any(|f| f["qr"] == "pairing-secret"));
    let without_credentials: Vec<Value> = from_process
        .into_iter()
        .filter(|f| f.get("qr").is_none())
        .collect();
    assert_eq!(from_core, without_credentials);
    assert!(from_core.iter().all(|f| f.get("session_id").is_none()));
}

/// The preview router keeps the in-process gateway's body limit on Doctor:
/// an oversized POST is refused before any check runs.
#[tokio::test]
async fn an_oversized_doctor_post_is_refused_by_both_routers() {
    use tower::ServiceExt as _;
    let harness = Harness::new(None);
    let in_process = axum::Router::new()
        .route(
            "/api/doctor",
            axum::routing::post(crate::api::handle_api_doctor),
        )
        .with_state(harness.state.clone())
        .layer(axum::Extension(CoreRpc::default()))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            crate::MAX_BODY_SIZE,
        ));
    let preview = crate::preview::router(
        harness.core.clone(),
        harness._dir.path().join("unused.sock"),
        None,
        tokio::sync::watch::channel(false).0,
        std::time::Duration::from_secs(crate::REQUEST_TIMEOUT_SECS),
    );
    for router in [in_process, preview] {
        let oversized = "x".repeat(crate::MAX_BODY_SIZE + 1);
        let request = axum::http::Request::post("/api/doctor")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::CONTENT_LENGTH, oversized.len())
            .body(axum::body::Body::from(oversized))
            .expect("request");
        let response = router.oneshot(request).await.expect("an answer");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}

#[cfg(unix)]
mod same_version_compatibility {
    use super::*;
    use std::sync::Mutex;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use zeroclaw_rpc_client::{EndpointOwner, Method};

    use zeroclaw_rpc_proto::feature::{
        DOCTOR_STATIC_ONLY, LOGS_FIELD_EQ, LOGS_QUERY_METADATA, LOGS_REPORT_DISABLED,
    };

    #[derive(Clone)]
    enum Advertisement {
        Omitted,
        TuiOnly,
        Without(&'static str),
    }

    impl Advertisement {
        fn lacks(&self, name: &str) -> bool {
            match self {
                Self::Omitted | Self::TuiOnly => name != "tui.client_kind",
                Self::Without(missing) => *missing == name,
            }
        }
    }

    struct LegacyCore {
        rpc: CoreRpc,
        methods: Arc<Mutex<Vec<String>>>,
        cancel: tokio_util::sync::CancellationToken,
        listener: tokio::task::JoinHandle<()>,
    }

    impl Drop for LegacyCore {
        fn drop(&mut self) {
            self.cancel.cancel();
            self.listener.abort();
        }
    }

    impl LegacyCore {
        fn serve(harness: &Harness, advertisement: Advertisement) -> Self {
            let endpoint = harness._dir.path().join("legacy.sock");
            let listener = tokio::net::UnixListener::bind(&endpoint).unwrap();
            let connector = InprocConnector::new(harness.cancel.clone());
            connector.bind(Arc::clone(&harness.ctx));
            let methods = Arc::new(Mutex::new(Vec::new()));
            let cancel = tokio_util::sync::CancellationToken::new();
            let listener = {
                let methods = Arc::clone(&methods);
                let cancel = cancel.clone();
                zeroclaw_spawn::spawn!(async move {
                    loop {
                        let accepted = tokio::select! {
                            biased;
                            _ = cancel.cancelled() => break,
                            accepted = listener.accept() => accepted,
                        };
                        let Ok((gateway, _)) = accepted else { break };
                        let Some(core) = crate::core_rpc::Dial::dial(&connector).await else {
                            break;
                        };
                        let (advertisement, methods, cancel) =
                            (advertisement.clone(), Arc::clone(&methods), cancel.clone());
                        zeroclaw_spawn::spawn!(async move {
                            tokio::select! {
                                _ = cancel.cancelled() => {},
                                _ = relay(gateway, core, advertisement, methods) => {},
                            }
                        });
                    }
                })
            };
            Self {
                rpc: CoreRpc::local(
                    endpoint,
                    EndpointOwner::SameAccount,
                    crate::core_rpc::VersionSkew::Refuse,
                ),
                methods,
                cancel,
                listener,
            }
        }

        async fn access(&self) -> CoreAccess {
            let access = self.rpc.access(&Harness::headers()).await.unwrap();
            let CoreAccess::Core(call) = &access else {
                panic!("in-process fallback")
            };
            let status = call.request(Method::Status, json!({})).await.unwrap();
            assert_eq!(status["server_version"], env!("CARGO_PKG_VERSION"));
            access
        }

        fn calls(&self, method: &str) -> usize {
            self.methods
                .lock()
                .unwrap()
                .iter()
                .filter(|m| m.as_str() == method)
                .count()
        }
    }

    async fn relay(
        gateway: tokio::net::UnixStream,
        core: tokio::io::DuplexStream,
        advertisement: Advertisement,
        methods: Arc<Mutex<Vec<String>>>,
    ) {
        let (gateway_reader, mut gateway_writer) = tokio::io::split(gateway);
        let (core_reader, mut core_writer) = tokio::io::split(core);
        let upstream_advertisement = advertisement.clone();
        let upstream = async move {
            let mut lines = BufReader::new(gateway_reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut frame: Value = serde_json::from_str(&line).unwrap();
                let method = frame["method"].as_str().unwrap_or_default().to_owned();
                methods.lock().unwrap().push(method.clone());
                if let Some(params) = frame["params"].as_object_mut() {
                    match method.as_str() {
                        "doctor/run" if upstream_advertisement.lacks(DOCTOR_STATIC_ONLY) => {
                            params.remove("static_only");
                        }
                        "status" => {
                            params.remove("overview");
                            params.remove("agent");
                        }
                        "logs/query" => {
                            if upstream_advertisement.lacks(LOGS_FIELD_EQ) {
                                params.remove("field_eq");
                            }
                            if upstream_advertisement.lacks(LOGS_REPORT_DISABLED) {
                                params.remove("report_disabled");
                            }
                        }
                        _ => {}
                    }
                }
                if core_writer
                    .write_all(format!("{frame}\n").as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
            }
            let _ = core_writer.shutdown().await;
        };
        let downstream = async move {
            let mut lines = BufReader::new(core_reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut frame: Value = serde_json::from_str(&line).unwrap();
                if frame["result"]["server_version"].is_string()
                    && frame["result"].get("tui_id").is_some()
                    && let Some(result) = frame["result"].as_object_mut()
                {
                    // Keep the real package version, so the real local pool's
                    // default version gate accepts this older implementation.
                    match &advertisement {
                        Advertisement::Omitted => {
                            result.remove("features");
                        }
                        Advertisement::TuiOnly => {
                            result.insert("features".into(), json!(["tui.client_kind"]));
                        }
                        Advertisement::Without(missing) => {
                            if let Some(features) =
                                result.get_mut("features").and_then(Value::as_array_mut)
                            {
                                features.retain(|name| name.as_str() != Some(missing));
                            }
                        }
                    }
                }
                if frame["result"]["events"].is_array()
                    && advertisement.lacks(LOGS_QUERY_METADATA)
                    && let Some(result) = frame["result"].as_object_mut()
                {
                    result.remove("persistence_enabled");
                    result.remove("daemon_started_at");
                    result.remove("attribution_keys");
                }
                if gateway_writer
                    .write_all(format!("{frame}\n").as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
            }
        };
        tokio::join!(upstream, downstream);
    }

    #[tokio::test]
    async fn an_older_core_cannot_turn_static_doctor_into_live_probes() {
        let provider = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::any())
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(json!({"data":[{"id":"probe-model"}]})),
            )
            .mount(&provider)
            .await;
        let harness = Harness::configured(None, |config| {
            let entry = config.providers.models.ensure("openai", "probe").unwrap();
            entry.uri = Some(format!("{}/v1/responses", provider.uri()));
            entry.wire_api = Some(zeroclaw_config::schema::WireApi::Responses);
            entry.api_key = Some("probe-fixture".into());
            entry.model = Some("probe-model".into());
        });
        let current = body_of(
            crate::api::handle_api_doctor(
                State(harness.state.clone()),
                Harness::headers(),
                harness.through_core().await,
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(current.0, StatusCode::OK);
        assert!(provider.received_requests().await.unwrap().is_empty());
        let old = LegacyCore::serve(&harness, Advertisement::Omitted);
        let answer = body_of(
            crate::api::handle_api_doctor(
                State(harness.state.clone()),
                Harness::headers(),
                old.access().await,
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(
            (answer.0, provider.received_requests().await.unwrap().len()),
            (StatusCode::SERVICE_UNAVAILABLE, 0)
        );
        assert_eq!(answer.1["code"], "core_capability_missing");
        assert_eq!(old.calls("doctor/run"), 0);
        use tower::ServiceExt as _;
        let router = crate::preview::router(
            old.rpc.clone(),
            harness._dir.path().join("legacy.sock"),
            None,
            tokio::sync::watch::channel(false).0,
            std::time::Duration::from_secs(crate::REQUEST_TIMEOUT_SECS),
        );
        let posted = body_of(
            router
                .oneshot(
                    axum::http::Request::builder()
                        .method("POST")
                        .uri("/api/doctor")
                        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(posted.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(posted.1["code"], "core_capability_missing");
        assert_eq!(old.calls("doctor/run"), 0);
        assert!(provider.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_older_core_with_persistence_off_reports_a_capability_gap() {
        let _writer = zeroclaw_log::__private_test_writer_lock();
        let dir = tempfile::tempdir().unwrap();
        install_log_writer(dir.path(), "none");
        let harness = Harness::new(None);
        let old = LegacyCore::serve(&harness, Advertisement::TuiOnly);
        let answer = body_of(
            crate::api_logs::handle_api_logs(
                State(harness.state.clone()),
                Harness::headers(),
                Query(HashMap::new()),
                old.access().await,
            )
            .await,
        )
        .await;
        assert_eq!(answer.0, StatusCode::SERVICE_UNAVAILABLE, "{}", answer.1);
        assert_eq!(answer.1["code"], "core_capability_missing");
        assert_eq!(old.calls("logs/query"), 0);
    }

    #[tokio::test]
    async fn each_requested_log_extension_is_proved_before_dispatch() {
        let _writer = zeroclaw_log::__private_test_writer_lock();
        let dir = tempfile::tempdir().unwrap();
        install_log_writer(dir.path(), "none");
        for missing in [LOGS_REPORT_DISABLED, LOGS_QUERY_METADATA, LOGS_FIELD_EQ] {
            let harness = Harness::new(None);
            let old = LegacyCore::serve(&harness, Advertisement::Without(missing));
            let params = if missing == LOGS_FIELD_EQ {
                HashMap::from([("agent_alias".into(), "alpha".into())])
            } else {
                HashMap::new()
            };
            let answer = body_of(
                crate::api_logs::handle_api_logs(
                    State(harness.state.clone()),
                    Harness::headers(),
                    Query(params),
                    old.access().await,
                )
                .await,
            )
            .await;
            assert_eq!(
                answer.0,
                StatusCode::SERVICE_UNAVAILABLE,
                "{missing}: {}",
                answer.1
            );
            assert_eq!(answer.1["code"], "core_capability_missing");
            assert_eq!(old.calls("logs/query"), 0, "{missing}");
        }
    }

    #[tokio::test]
    async fn unused_log_filter_support_is_not_required_for_an_empty_page() {
        let _writer = zeroclaw_log::__private_test_writer_lock();
        let dir = tempfile::tempdir().unwrap();
        install_log_writer(dir.path(), "none");
        let harness = Harness::new(None);
        let old = LegacyCore::serve(&harness, Advertisement::Without(LOGS_FIELD_EQ));
        let answer = body_of(
            crate::api_logs::handle_api_logs(
                State(harness.state.clone()),
                Harness::headers(),
                Query(HashMap::new()),
                old.access().await,
            )
            .await,
        )
        .await;
        assert_eq!(answer.0, StatusCode::OK, "{}", answer.1);
        assert_eq!(answer.1["persistence_enabled"], false);
        assert_eq!(old.calls("logs/query"), 1);
    }

    #[tokio::test]
    async fn invalid_log_queries_keep_their_status_without_extension_support() {
        let harness = Harness::new(None);
        let old = LegacyCore::serve(&harness, Advertisement::TuiOnly);
        let answer = body_of(
            crate::api_logs::handle_api_logs(
                State(harness.state.clone()),
                Harness::headers(),
                Query(HashMap::from([("no_such".into(), "x".into())])),
                old.access().await,
            )
            .await,
        )
        .await;
        assert_eq!(answer.0, StatusCode::BAD_REQUEST);
        assert_eq!(old.calls("logs/query"), 0);
    }
}
