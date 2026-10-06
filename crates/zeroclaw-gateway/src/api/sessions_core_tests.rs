//! The sessions routes with a core attached.
//!
//! In the in-process gateway every sessions route stays in-process. These
//! tests attach the daemon's real in-process connector over one session
//! store, opened twice as in production: once by the core, once by the
//! gateway. They pin why, one test per hazard a core-backed version had: a
//! listing the core scopes to the gateway's own connection, a revoked bearer
//! cancelling a turn, the core acting on a competing row, a delete returning
//! while the gateway's own turn still runs, and a transcript too large for one
//! RPC frame.
//!
//! The separate zeroclaw-gw, which runs no turns, serves the per-session
//! routes through the core by exact stored key. The last section pins how it
//! reads a transcript across pages; the preview's tests hold its parity.

use super::tests::{response_json, test_state_with_session_backend};
use super::*;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::http::HeaderValue;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio_util::sync::CancellationToken;
use zeroclaw_config::schema::Config;
use zeroclaw_infra::session_backend::{SessionBackend, SessionContext};
use zeroclaw_providers::ChatMessage;
use zeroclaw_rpc_client::Method;
use zeroclaw_rpc_proto::types::SessionListResult;
use zeroclaw_runtime::rpc::context::RpcContext;
use zeroclaw_runtime::rpc::inproc::InprocConnector;

use crate::core_rpc::{CoreAccess, CoreRpc, Dial, DialFuture};

const OPERATOR_TOKEN: &str = "zc_gw_operator";

/// A core and a gateway over one store, with the gateway's own handle.
struct Stores {
    _tmp: tempfile::TempDir,
    ctx: Arc<RpcContext>,
    core: CoreRpc,
    stop: CancellationToken,
    state: AppState,
    backend: Arc<dyn SessionBackend>,
}

impl Drop for Stores {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl Stores {
    /// Share the core's live pairing authority with the gateway, as a
    /// supervised gateway does: pairing required, revocation seen by both.
    fn with_shared_pairing(mut self) -> Self {
        self.state.pairing = Arc::clone(self.ctx.auth.pairing());
        self
    }
}

fn open_store(config: &Config) -> Arc<dyn SessionBackend> {
    zeroclaw_infra::make_session_backend(&config.data_dir, &config.channels.session_backend)
        .expect("open the session store")
}

fn stores() -> Stores {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = Config {
        data_dir: tmp.path().to_path_buf(),
        config_path: tmp.path().join("config.toml"),
        ..Config::default()
    };
    config.gateway.require_pairing = true;
    config.gateway.paired_tokens = vec![OPERATOR_TOKEN.into()];
    // The daemon's TUI identity signing key. The in-process connector is a
    // non-local caller, and the core refuses its `initialize` while signing
    // is off.
    std::fs::write(tmp.path().join(".secret_key"), "42".repeat(32)).expect("signing key");
    let sessions = Arc::new(zeroclaw_runtime::rpc::session::SessionStore::new(
        16,
        Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
            4, 10, 60,
        )),
    ));
    let mut ctx = RpcContext::for_live_test(config.clone(), sessions);
    assert!(ctx.tui_registry.signing_is_enabled());
    Arc::get_mut(&mut ctx)
        .expect("a fresh context has one owner")
        .session_backend = Some(open_store(&config));
    let stop = CancellationToken::new();
    let connector = InprocConnector::new(stop.clone());
    connector.bind(Arc::clone(&ctx));
    let backend = open_store(&config);
    let state = test_state_with_session_backend(config, Arc::clone(&backend));
    Stores {
        _tmp: tmp,
        ctx,
        core: CoreRpc::inproc(connector, || true),
        stop,
        state,
        backend,
    }
}

fn seed(backend: &dyn SessionBackend) {
    // A named dashboard chat with a turn in flight.
    backend
        .append("gw_alpha", &ChatMessage::user("hi"))
        .unwrap();
    backend
        .append("gw_alpha", &ChatMessage::assistant("hello"))
        .unwrap();
    backend.set_session_agent_alias("gw_alpha", "main").unwrap();
    backend.set_session_name("gw_alpha", "Alpha").unwrap();
    backend
        .set_session_state("gw_alpha", "running", Some("turn-7"))
        .unwrap();
    // A channel session, attributable by its channel alone.
    backend
        .append("discord.room_1", &ChatMessage::user("from discord"))
        .unwrap();
    backend
        .set_session_context(
            "discord.room_1",
            SessionContext {
                channel_id: Some("discord.ops"),
                ..SessionContext::default()
            },
        )
        .unwrap();
    // A session another client opened over RPC keeps its prefix here.
    backend
        .append("rpc_zc1", &ChatMessage::user("from zerocode"))
        .unwrap();
    backend.set_session_agent_alias("rpc_zc1", "main").unwrap();
    // No alias and no channel: an orphan neither path lists.
    backend
        .append("gw_orphan", &ChatMessage::user("orphan"))
        .unwrap();
}

fn bearer(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    );
    headers
}

async fn through(core: &CoreRpc, token: &str) -> CoreAccess {
    let access = core
        .access(&bearer(token))
        .await
        .expect("the bearer opens a core connection");
    assert!(matches!(access, CoreAccess::Core(_)));
    access
}

async fn answer(response: impl IntoResponse) -> (StatusCode, Value) {
    let response = response.into_response();
    let status = response.status();
    (status, response_json(response).await)
}

fn register_turn(state: &AppState, cancel_key: &str) -> CancellationToken {
    let token = CancellationToken::new();
    state
        .cancel_tokens
        .lock()
        .expect("cancel_tokens lock")
        .insert(cancel_key.to_string(), Arc::new(token.clone()));
    token
}

// ── Listing through the credential-bound core connection ──────────

/// The credential-bound duplex and the gateway's local store list the same
/// attributable rows for the operator, including sessions opened elsewhere.
#[tokio::test]
async fn the_listing_is_the_same_through_the_core() {
    let stores = stores().with_shared_pairing();
    seed(&*stores.backend);

    let CoreAccess::Core(call) = through(&stores.core, OPERATOR_TOKEN).await else {
        unreachable!()
    };
    let core_listed: SessionListResult = call
        .call(Method::SessionList, json!({}))
        .await
        .expect("the core lists");
    assert!(
        core_listed.sessions.len() == 3,
        "the operator lists every attributable row: {:?}",
        core_listed.sessions
    );

    let (status, body) = answer(
        handle_api_sessions_list(
            State(stores.state.clone()),
            bearer(OPERATOR_TOKEN),
            through(&stores.core, OPERATOR_TOKEN).await,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let rows = body["sessions"].as_array().expect("a sessions array");
    let row = |key: &str| {
        rows.iter()
            .find(|row| row["session_key"] == key)
            .unwrap_or_else(|| panic!("{key} is listed"))
    };
    assert_eq!(rows.len(), 3, "every attributable session, and no orphan");
    assert_eq!(row("gw_alpha")["session_id"], "alpha");
    assert_eq!(row("gw_alpha")["name"], "Alpha");
    assert_eq!(
        row("rpc_zc1")["session_id"],
        "rpc_zc1",
        "only the gateway prefix is stripped for display"
    );
    assert_eq!(row("discord.room_1")["channel_id"], "discord.ops");
    assert_eq!(row("discord.room_1")["agent_alias"], Value::Null);
}

// ── Why the per-session routes stay in-process ───────────────────

/// A revoked pairing token that still has a pooled core connection cannot
/// cancel a turn through DELETE: the route checks the live pairing authority
/// before it touches the gateway's turn.
#[tokio::test]
async fn a_revoked_bearer_with_a_pooled_connection_cancels_nothing() {
    let stores = stores().with_shared_pairing();
    seed(&*stores.backend);
    let turn = register_turn(&stores.state, "gw_alpha");

    // Warm the pool: a core request with the operator's bearer.
    let CoreAccess::Core(call) = through(&stores.core, OPERATOR_TOKEN).await else {
        unreachable!()
    };
    call.request(Method::Status, json!({}))
        .await
        .expect("the operator's status");

    assert!(stores.ctx.auth.pairing().revoke_token(OPERATOR_TOKEN));
    let (status, body) = answer(
        handle_api_session_delete(
            State(stores.state.clone()),
            bearer(OPERATOR_TOKEN),
            Path("alpha".to_string()),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert!(!turn.is_cancelled(), "a refused caller cancels nothing");
    assert!(stores.backend.session_exists("gw_alpha"));
}

/// With `gw_alpha` and a same-owner `rpc_gw_alpha`, the core never answers
/// for the gateway's row when asked for the id `gw_alpha`: it resolves the id
/// to the RPC row, or refuses it to a caller that does not own it. The routes
/// act on the row this gateway selected, because they do not ask the core to
/// resolve it.
#[tokio::test]
async fn a_competing_rpc_row_never_answers_for_the_gateway_row() {
    let stores = stores();
    seed(&*stores.backend);
    stores
        .backend
        .append(
            "rpc_gw_alpha",
            &ChatMessage::user("different RPC conversation"),
        )
        .unwrap();

    // The hazard: asked for `gw_alpha`, the core never reads the gateway's
    // row. It reads the RPC row, or refuses a caller that does not own it.
    let CoreAccess::Core(call) = through(&stores.core, OPERATOR_TOKEN).await else {
        unreachable!()
    };
    let core_read = call
        .request(Method::SessionMessages, json!({"session_id": "gw_alpha"}))
        .await
        .unwrap();
    assert_eq!(
        core_read["messages"][0]["content"],
        "different RPC conversation"
    );

    let (status, body) = answer(
        handle_api_session_messages(
            State(stores.state.clone()),
            bearer(OPERATOR_TOKEN),
            Path("alpha".to_string()),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let contents: Vec<&str> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["content"].as_str().unwrap())
        .collect();
    assert_eq!(contents, ["hi", "hello"]);

    let (status, body) = answer(
        handle_api_session_state(
            State(stores.state.clone()),
            bearer(OPERATOR_TOKEN),
            Path("alpha".to_string()),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "running");
    assert_eq!(body["turn_id"], "turn-7");

    let (status, body) = answer(
        handle_api_session_delete(
            State(stores.state.clone()),
            bearer(OPERATOR_TOKEN),
            Path("alpha".to_string()),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !stores.backend.session_exists("gw_alpha"),
        "the selected row goes"
    );
    assert!(
        stores.backend.session_exists("rpc_gw_alpha"),
        "the competing row stays"
    );
}

/// A delete waits for the gateway's own turn on the session to finish
/// before it answers, and a bearer the pairing authority does not know (an
/// administrator's OIDC token, say) neither deletes nor touches that turn.
#[tokio::test]
async fn a_delete_answers_only_after_the_gateways_turn_has_settled() {
    let stores = stores().with_shared_pairing();
    seed(&*stores.backend);
    let turn = register_turn(&stores.state, "gw_alpha");
    let settled = Arc::new(AtomicBool::new(false));
    let permit = stores
        .state
        .session_queue
        .acquire("gw_alpha")
        .await
        .expect("the turn holds the session");
    let worker = {
        let turn = turn.clone();
        let settled = Arc::clone(&settled);
        zeroclaw_spawn::spawn!(async move {
            turn.cancelled().await;
            // Unwinding takes a moment; the permit is held throughout.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            settled.store(true, Ordering::SeqCst);
            drop(permit);
        })
    };

    let (status, _) = answer(
        handle_api_session_delete(
            State(stores.state.clone()),
            bearer("zc_gw_admin_from_elsewhere"),
            Path("alpha".to_string()),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!turn.is_cancelled());
    assert!(stores.backend.session_exists("gw_alpha"));

    let (status, body) = answer(
        handle_api_session_delete(
            State(stores.state.clone()),
            bearer(OPERATOR_TOKEN),
            Path("alpha".to_string()),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        settled.load(Ordering::SeqCst),
        "the delete answered while the turn was still running"
    );
    assert!(!stores.backend.session_exists("gw_alpha"));
    worker.await.expect("worker");
}

/// A transcript larger than one RPC frame (8 MiB) is still served whole.
#[tokio::test]
async fn a_transcript_larger_than_an_rpc_frame_is_served_whole() {
    let stores = stores();
    let chunk = "x".repeat(60 * 1024);
    for _ in 0..150 {
        stores
            .backend
            .append("gw_big", &ChatMessage::user(&chunk))
            .unwrap();
    }
    let (status, body) = answer(
        handle_api_session_messages(
            State(stores.state.clone()),
            bearer(OPERATOR_TOKEN),
            Path("big".to_string()),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["messages"].as_array().unwrap().len(), 150);
}

// ── A scripted core: what it was asked, and what it answers ──────

#[derive(Default)]
struct ScriptedCore {
    methods: Mutex<Vec<String>>,
    /// The params of each call after `initialize`, in order.
    params: Mutex<Vec<Value>>,
    /// What each call after `initialize` answers, in order: an object with a
    /// `result` or an `error`. A call with no answer left is refused.
    answers: Mutex<std::collections::VecDeque<Value>>,
}

struct Scripted(Arc<ScriptedCore>);

impl Dial for Scripted {
    fn dial(&self) -> DialFuture<'_> {
        let core = Arc::clone(&self.0);
        Box::pin(async move {
            let (client, server) = tokio::io::duplex(64 * 1024);
            zeroclaw_spawn::spawn!(serve_scripted(core, server));
            Some(client)
        })
    }
}

async fn serve_scripted(core: Arc<ScriptedCore>, stream: DuplexStream) {
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let frame: Value = serde_json::from_str(&line).expect("client frames are JSON");
        let id = frame["id"].clone();
        let method = frame["method"].as_str().unwrap_or_default().to_owned();
        let answer = if method == "initialize" {
            json!({"jsonrpc": "2.0", "id": id, "result": {
                "protocol_version": 1, "server_version": "scripted", "server_pid": 1,
                "principal_id": "shared-operator",
            }})
        } else {
            core.methods.lock().expect("methods lock").push(method);
            core.params
                .lock()
                .expect("params lock")
                .push(frame["params"].clone());
            match core.answers.lock().expect("answers lock").pop_front() {
                Some(Value::Object(mut scripted)) => {
                    scripted.insert("jsonrpc".into(), json!("2.0"));
                    scripted.insert("id".into(), id);
                    Value::Object(scripted)
                }
                _ => json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": {"code": -32601, "message": "unscripted"},
                }),
            }
        };
        if write
            .write_all(format!("{answer}\n").as_bytes())
            .await
            .is_err()
        {
            break;
        }
    }
}

// ── zeroclaw-gw's transcript read: pages, oldest first ───────────

/// The row the scripted pages come from, and when it was created.
const ROW: (&str, &str) = ("gw_alpha", "2026-10-01T00:00:00+00:00");

/// A `session/messages` page of `total` entries starting at `start`, read
/// from `row`.
fn page_of(row: (&str, &str), total: usize, start: usize, contents: &[&str]) -> Value {
    let messages: Vec<Value> = contents
        .iter()
        .map(|content| json!({"role": "user", "content": content, "kind": "message"}))
        .collect();
    json!({"result": {
        "session_id": "alpha", "messages": messages, "total": total, "start": start,
        "session_key": row.0, "session_created_at": row.1,
        "session_revision": "scripted-complete-history",
    }})
}

/// A page of [`ROW`].
fn transcript_page(total: usize, start: usize, contents: &[&str]) -> Value {
    page_of(ROW, total, start, contents)
}

/// `GET /api/sessions/alpha/messages` as zeroclaw-gw reads it from a core
/// that gives `answers`.
async fn read_scripted(answers: Vec<Value>) -> (Arc<ScriptedCore>, (StatusCode, Value)) {
    let scripted = Arc::new(ScriptedCore::default());
    scripted.answers.lock().unwrap().extend(answers);
    let core = CoreRpc::over_dialer(Scripted(Arc::clone(&scripted)));
    let CoreAccess::Core(call) = through(&core, OPERATOR_TOKEN).await else {
        unreachable!("through() asserts a core connection")
    };
    let response = match api_session_messages_through_core(&call, "alpha").await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    (scripted, answer(response).await)
}

fn contents(body: &Value) -> Vec<&str> {
    body["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .map(|row| row["content"].as_str().expect("content"))
        .collect()
}

#[tokio::test]
async fn a_core_without_transcript_revision_support_is_refused() {
    for revision in [Value::Null, json!("")] {
        let mut page = transcript_page(2, 0, &["a", "b"]);
        page["result"]["session_revision"] = revision;
        let (scripted, (status, body)) = read_scripted(vec![page]).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(body["code"], "core_capability_missing");
        assert_eq!(scripted.params.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn a_same_timestamp_transcript_replacement_restarts_paging() {
    fn revision_page(revision: &str, start: usize, contents: &[&str]) -> Value {
        let mut page = page_of(ROW, 4, start, contents);
        page["result"]["session_revision"] = json!(revision);
        page
    }
    let (scripted, (status, body)) = read_scripted(vec![
        revision_page("original", 2, &["old-c", "old-d"]),
        revision_page("replacement", 0, &["new-a", "new-b"]),
        revision_page("replacement", 2, &["new-c", "new-d"]),
        revision_page("replacement", 0, &["new-a", "new-b"]),
    ])
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(contents(&body), ["new-a", "new-b", "new-c", "new-d"]);
    assert_eq!(scripted.params.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn a_paged_transcript_is_asked_for_by_exact_key_and_served_oldest_first() {
    let (scripted, (status, body)) = read_scripted(vec![
        transcript_page(5, 3, &["d", "e"]),
        transcript_page(5, 1, &["b", "c"]),
        transcript_page(5, 0, &["a"]),
    ])
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(contents(&body), ["a", "b", "c", "d", "e"]);
    assert_eq!(body["session_id"], "alpha");
    assert_eq!(body["session_persistence"], true);
    assert_eq!(
        body["messages"][0],
        json!({"role": "user", "content": "a", "created_at": null})
    );

    let params = scripted.params.lock().unwrap().clone();
    let before: Vec<Value> = params.iter().map(|p| p["before_index"].clone()).collect();
    assert_eq!(before, [Value::Null, json!(3), json!(1)]);
    // The newest page is resolved from the candidates; every later page names
    // only the row the newest page read.
    let keys: Vec<Value> = params.iter().map(|p| p["session_keys"].clone()).collect();
    assert_eq!(
        keys,
        [
            json!(["alpha", "gw_alpha"]),
            json!(["gw_alpha"]),
            json!(["gw_alpha"])
        ]
    );
    for sent in &params {
        assert_eq!(sent["session_id"], "alpha");
        assert_eq!(sent["max_bytes"], json!(CORE_MESSAGE_PAGE_BYTES));
        assert!(
            sent.get("cursor").is_none(),
            "never ACP cursor mode: {sent}"
        );
    }
}

#[tokio::test]
async fn a_page_from_another_row_starts_the_read_over_rather_than_mixing_rows() {
    let recreated = (ROW.0, "2026-10-01T00:00:09+00:00");
    let (scripted, (status, body)) = read_scripted(vec![
        transcript_page(4, 2, &["c", "d"]),
        // The row was removed and recreated under the same key, with as many
        // entries: only its creation time tells it apart.
        page_of(recreated, 4, 0, &["new-a", "new-b"]),
        page_of(recreated, 2, 0, &["new-a", "new-b"]),
    ])
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(contents(&body), ["new-a", "new-b"]);
    assert_eq!(scripted.params.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn a_core_that_does_not_name_its_row_is_not_paged_blind() {
    let mut nameless = transcript_page(4, 2, &["c", "d"]);
    nameless["result"]
        .as_object_mut()
        .expect("a result")
        .remove("session_key");
    let (scripted, (status, body)) = read_scripted(vec![nameless]).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["code"], "core_error");
    assert_eq!(
        scripted.params.lock().unwrap().len(),
        1,
        "no later page is asked for without a row to bind it to"
    );
}

#[tokio::test]
async fn a_transcript_rewritten_between_pages_is_read_again_from_the_newest() {
    let (scripted, (status, body)) = read_scripted(vec![
        transcript_page(5, 3, &["d", "e"]),
        // Compacted underneath the walk: fewer entries than the first page saw.
        transcript_page(2, 0, &["summary"]),
        transcript_page(2, 0, &["summary", "f"]),
    ])
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(contents(&body), ["summary", "f"]);
    assert_eq!(scripted.params.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn a_transcript_that_keeps_changing_is_a_conflict_not_a_mix() {
    let mut answers = Vec::new();
    for _ in 0..CORE_MESSAGE_READ_ATTEMPTS {
        answers.push(transcript_page(5, 3, &["d", "e"]));
        answers.push(transcript_page(2, 0, &["summary"]));
    }
    let (scripted, (status, body)) = read_scripted(answers).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "conflict");
    assert_eq!(
        scripted.params.lock().unwrap().len(),
        2 * CORE_MESSAGE_READ_ATTEMPTS
    );
}

#[tokio::test]
async fn a_core_page_that_makes_no_progress_is_not_asked_again() {
    let (scripted, (status, body)) = read_scripted(vec![
        transcript_page(5, 3, &["d", "e"]),
        transcript_page(5, 3, &["d", "e"]),
    ])
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["code"], "core_error");
    assert_eq!(scripted.params.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_message_no_page_can_carry_is_named_as_a_bad_gateway() {
    let (_, (status, body)) = read_scripted(vec![json!({"error": {
        "code": zeroclaw_api::jsonrpc::error_codes::INVALID_PARAMS,
        "message": "message 4 is 9000000 bytes, more than max_bytes",
        "data": {"reason": "entry_exceeds_max_bytes", "index": 4, "bytes": 9_000_000},
    }})])
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["code"], "message_too_large");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| error.starts_with("message 4 is 9000000 bytes")),
        "{body}"
    );

    // Any other invalid-params refusal is the core's, passed on as such.
    let (_, (status, body)) = read_scripted(vec![json!({"error": {
        "code": zeroclaw_api::jsonrpc::error_codes::INVALID_PARAMS,
        "message": "session_keys must list 1 to 4 non-empty keys",
    }})])
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_params");
}
