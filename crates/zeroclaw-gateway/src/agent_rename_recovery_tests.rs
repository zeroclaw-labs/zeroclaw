//! Gateway and RPC coverage for committed agent-rename recovery.
//!
//! These tests call the public handlers and the public RPC dispatcher. They
//! do not reach into the recovery module.

use std::sync::Arc;

use axum::extract::{Json, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use http_body_util::BodyExt;
use zeroclaw_config::alias_refs::{self, AliasKind};
use zeroclaw_config::schema::{AliasedAgentConfig, Config};
use zeroclaw_runtime::rpc::context::RpcContext;
use zeroclaw_runtime::rpc::dispatch::RpcDispatcher;
use zeroclaw_runtime::rpc::session::SessionStore;
use zeroclaw_runtime::rpc::transport::{RpcTransport, TransportKind};

use crate::api::test_state;
use crate::api_config::{
    MapKeyQuery, RenameMapKeyBody, handle_delete_map_key, handle_delete_plan, handle_map_key,
    handle_rename_map_key,
};

fn fixture(dir: &std::path::Path, alias: &str) -> Config {
    let mut config = Config {
        config_path: dir.join("config.toml"),
        data_dir: dir.join("data"),
        ..Config::default()
    };
    std::fs::create_dir_all(&config.data_dir).unwrap();
    config.agents.insert(
        alias.to_string(),
        AliasedAgentConfig {
            risk_profile: "default".into(),
            ..AliasedAgentConfig::default()
        },
    );
    config
        .risk_profiles
        .entry("default".into())
        .or_default()
        .allowed_commands = vec!["echo".into()];
    config.runtime_profiles.entry("default".into()).or_default();
    config
}

fn commit_alias(config: &mut Config, from: &str, to: &str) {
    alias_refs::rename_with_cascade(config, &AliasKind::Agent, from, to).unwrap();
}

async fn response_json(response: Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or_else(
        |_| serde_json::json!({ "raw": String::from_utf8_lossy(&bytes).to_string() }),
    );
    (status, json)
}

async fn rename(state: &crate::AppState, from: &str, to: &str) -> (StatusCode, serde_json::Value) {
    let response = Box::pin(handle_rename_map_key(
        State(state.clone()),
        None,
        Json(RenameMapKeyBody {
            path: "agents".into(),
            from: from.into(),
            to: to.into(),
        }),
    ))
    .await;
    response_json(response).await
}

async fn create_agent(state: &crate::AppState, alias: &str) -> (StatusCode, serde_json::Value) {
    let response = Box::pin(handle_map_key(
        State(state.clone()),
        None,
        Query(MapKeyQuery {
            path: "agents".into(),
            key: alias.into(),
        }),
    ))
    .await;
    response_json(response).await
}

async fn delete_agent(state: &crate::AppState, alias: &str) -> (StatusCode, serde_json::Value) {
    let response = Box::pin(handle_delete_map_key(
        State(state.clone()),
        None,
        Query(MapKeyQuery {
            path: "agents".into(),
            key: alias.into(),
        }),
    ))
    .await;
    response_json(response).await
}

async fn delete_plan(state: &crate::AppState, alias: &str) -> (StatusCode, serde_json::Value) {
    let response = Box::pin(handle_delete_plan(
        State(state.clone()),
        Query(MapKeyQuery {
            path: "agents".into(),
            key: alias.into(),
        }),
    ))
    .await;
    response_json(response).await
}

/// Bring the alias a rename retired back into the live config, as a hand edit
/// would, around the create guards.
fn configure_again(state: &crate::AppState, alias: &str) {
    state.config.write().agents.insert(
        alias.to_string(),
        AliasedAgentConfig {
            risk_profile: "default".into(),
            ..AliasedAgentConfig::default()
        },
    );
}

fn message_of(json: &serde_json::Value) -> String {
    json["message"].as_str().unwrap_or("").to_string()
}

/// Whether a refusal says an unfinished rename is why: the alias is retired,
/// or a rename is still converging into it.
fn names_the_unfinished_rename(message: &str) -> bool {
    message.contains("retired") || message.contains("unfinished rename")
}

/// Assert an RPC response is the refusal an unfinished rename makes: invalid
/// params, with a message that says why.
fn assert_rpc_rename_refusal(response: &serde_json::Value) {
    assert_eq!(
        response["error"]["code"],
        zeroclaw_api::jsonrpc::error_codes::INVALID_PARAMS,
        "{response}"
    );
    let message = response["error"]["message"].as_str().unwrap_or("");
    assert!(names_the_unfinished_rename(message), "{response}");
}

struct PipeTransport {
    frames: std::collections::VecDeque<String>,
    writer: tokio::sync::mpsc::Sender<String>,
}

#[async_trait::async_trait]
impl RpcTransport for PipeTransport {
    fn writer(&self) -> tokio::sync::mpsc::Sender<String> {
        self.writer.clone()
    }

    async fn next_frame(&mut self) -> Option<String> {
        self.frames.pop_front()
    }

    fn peer_label(&self) -> String {
        "committed-rename-test".into()
    }

    fn kind(&self) -> TransportKind {
        TransportKind::Local
    }
}

fn rpc_frame(id: u64, method: &str, params: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
    .to_string()
}

/// Stack for the thread that drives the dispatcher: the 8 MiB the daemon gives
/// its runtime workers (`async_main` in the root crate). Unoptimized builds of
/// the config write methods, dispatched through `process_line`, need more than
/// the 2 MiB a test thread gets.
const RPC_THREAD_STACK_BYTES: usize = 8 * 1024 * 1024;

async fn rpc_roundtrip(
    config: Config,
    calls: Vec<(u64, String, serde_json::Value)>,
) -> Vec<serde_json::Value> {
    std::thread::Builder::new()
        .name("committed-rename-rpc".into())
        .stack_size(RPC_THREAD_STACK_BYTES)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("rpc test runtime")
                .block_on(drive_dispatcher(config, calls))
        })
        .expect("spawn the rpc thread")
        .join()
        .expect("the rpc thread finishes")
}

async fn drive_dispatcher(
    config: Config,
    calls: Vec<(u64, String, serde_json::Value)>,
) -> Vec<serde_json::Value> {
    let sessions = Arc::new(SessionStore::new(
        8,
        Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
            8, 30, 600,
        )),
    ));
    let ctx = RpcContext::for_live_test(config, sessions);
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let mut dispatcher = RpcDispatcher::new(ctx, tx.clone(), "committed-rename-test".into());
    let mut frames = std::collections::VecDeque::new();
    frames.push_back(rpc_frame(
        1,
        "initialize",
        serde_json::json!({ "protocol_version": 1 }),
    ));
    for (id, method, params) in calls {
        frames.push_back(rpc_frame(id, &method, params));
    }
    let mut transport = PipeTransport { frames, writer: tx };
    dispatcher.run(&mut transport).await;
    let mut out = Vec::new();
    while let Ok(line) = rx.try_recv() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
            out.push(value);
        }
    }
    out
}

fn rpc_by_id(messages: &[serde_json::Value], id: u64) -> serde_json::Value {
    messages
        .iter()
        .find(|message| message["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("missing rpc response {id}: {messages:?}"))
}

/// A rename of `agent_a` to `agent_b` whose config commit landed while its
/// workspace could not follow: a file stands where `<install>/agents/agent_b`
/// must be a directory. The recovery record stays open. Returns the gateway
/// state, the stranded workspace, and the blocker.
async fn blocked_rename(
    dir: &std::path::Path,
) -> (crate::AppState, std::path::PathBuf, std::path::PathBuf) {
    let config = fixture(dir, "agent_a");
    let old_ws = config.agent_workspace_dir("agent_a");
    std::fs::create_dir_all(&old_ws).unwrap();
    std::fs::write(old_ws.join("marker"), b"keep").unwrap();
    let blocker = dir.join("agents").join("agent_b");
    std::fs::write(&blocker, b"x").unwrap();

    let state = test_state(config);
    let (status, json) = rename(&state, "agent_a", "agent_b").await;
    assert!(status.is_success(), "the config commit stands: {json}");
    assert!(
        json["warnings"].as_array().is_some_and(|w| !w.is_empty()),
        "the stranded workspace is reported: {json}"
    );
    (state, old_ws, blocker)
}

#[tokio::test]
async fn committed_rename_unreadable_cron_fails_closed_and_blocks_reuse() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = fixture(tmp.path(), "agent_a");
    commit_alias(&mut config, "agent_a", "agent_b");
    let cron_db = config.data_dir.join("cron").join("jobs.db");
    std::fs::create_dir_all(&cron_db).unwrap();

    let state = test_state(config);
    let (status, json) = rename(&state, "agent_a", "agent_b").await;
    let message = message_of(&json);
    assert_ne!(
        status,
        StatusCode::NOT_FOUND,
        "an unreadable cron store must not look like a missing alias: {status} {json}"
    );
    assert!(
        !message.contains("is not configured") && !message.contains("alias not found"),
        "unreadable recovery must stay retryable, got {message}"
    );

    let (create_status, create_json) = create_agent(&state, "agent_a").await;
    assert!(
        !create_status.is_success(),
        "the retired alias stays blocked while recovery is outstanding: {create_json}"
    );
}

#[tokio::test]
async fn committed_rename_blocked_workspace_refuses_alias_reuse_until_converged() {
    let tmp = tempfile::tempdir().unwrap();
    let config = fixture(tmp.path(), "agent_a");
    let old_ws = config.agent_workspace_dir("agent_a");
    std::fs::create_dir_all(&old_ws).unwrap();
    std::fs::write(old_ws.join("marker"), b"keep").unwrap();
    let blocker = tmp.path().join("agents").join("agent_b");
    std::fs::create_dir_all(blocker.parent().unwrap()).unwrap();
    std::fs::write(&blocker, b"x").unwrap();

    let state = test_state(config);
    let (status, json) = rename(&state, "agent_a", "agent_b").await;
    assert!(
        status.is_success(),
        "the config commit is kept when only the workspace move lags: {json}"
    );
    assert!(
        old_ws.join("marker").is_file(),
        "failed workspace move leaves the marker in place"
    );

    let (blocked_status, blocked_json) = create_agent(&state, "agent_a").await;
    assert!(
        !blocked_status.is_success(),
        "open recovery refuses the retired alias: {blocked_json}"
    );
    let (other_status, other_json) = create_agent(&state, "agent_c").await;
    assert!(
        other_status.is_success(),
        "a different alias is not stranded: {other_json}"
    );

    std::fs::remove_file(&blocker).unwrap();
    std::fs::remove_dir_all(&old_ws).unwrap();
    let (still_status, still_json) = create_agent(&state, "agent_a").await;
    assert!(
        !still_status.is_success(),
        "wiping residue out of band does not clear recovery: {still_json}"
    );

    let (retry_status, retry_json) = rename(&state, "agent_a", "agent_b").await;
    assert!(
        retry_status.is_success(),
        "retry converges once followers are readable: {retry_json}"
    );
    let warnings = retry_json["warnings"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        warnings.is_empty(),
        "a clean retry clears recovery, got {retry_json}"
    );
    let (freed_status, freed_json) = create_agent(&state, "agent_a").await;
    assert!(
        freed_status.is_success(),
        "reuse is allowed after convergence: {freed_json}"
    );
}

#[tokio::test]
async fn committed_rename_sqlite_followers_converge_and_then_allow_reuse() {
    use zeroclaw_memory::Memory;

    let tmp = tempfile::tempdir().unwrap();
    let mut config = fixture(tmp.path(), "agent_a");
    let old_ws = config.agent_workspace_dir("agent_a");
    std::fs::create_dir_all(&old_ws).unwrap();
    std::fs::write(old_ws.join("marker"), b"ws").unwrap();
    zeroclaw_runtime::cron::add_job(&config, "agent_a", "* * * * *", "echo hi").unwrap();

    let memory = Arc::new(
        zeroclaw_memory::SqliteMemory::new("agent_a", &config.data_dir).expect("sqlite memory"),
    );
    memory.ensure_agent_uuid("agent_a").await.unwrap();
    let sessions = zeroclaw_infra::make_session_backend(&config.data_dir, "sqlite").unwrap();
    sessions
        .set_session_agent_alias("sess-1", "agent_a")
        .unwrap();
    {
        let acp =
            zeroclaw_infra::acp_session_store::AcpSessionStore::new(&config.data_dir).unwrap();
        acp.create_session("acp-1", "agent_a", old_ws.to_str().unwrap_or("/tmp"), None)
            .unwrap();
    }

    commit_alias(&mut config, "agent_a", "agent_b");
    let mut state = test_state(config.clone());
    state.mem = memory.clone();
    state.session_backend = Some(sessions.clone());

    let (status, json) = rename(&state, "agent_a", "agent_b").await;
    assert!(status.is_success(), "committed residue resumes: {json}");
    let warnings = json["warnings"].as_array().cloned().unwrap_or_default();
    assert!(
        warnings.is_empty(),
        "followers converge without leftover warnings: {json}"
    );
    assert!(
        state
            .config
            .read()
            .agent_workspace_dir("agent_b")
            .join("marker")
            .is_file()
    );
    assert!(!old_ws.exists());
    assert_eq!(memory.count_agent("agent_a").await.unwrap(), 0);
    assert_eq!(memory.count_agent("agent_b").await.unwrap(), 1);
    assert_eq!(
        zeroclaw_runtime::cron::list_jobs_by_agent(&config, "agent_b")
            .unwrap()
            .len(),
        1
    );
    assert!(
        zeroclaw_runtime::cron::list_jobs_by_agent(&config, "agent_a")
            .unwrap()
            .is_empty()
    );
    let acp = zeroclaw_infra::acp_session_store::AcpSessionStore::new(&config.data_dir).unwrap();
    assert_eq!(acp.list_sessions_by_agent("agent_b").unwrap().len(), 1);
    assert!(acp.list_sessions_by_agent("agent_a").unwrap().is_empty());
    assert_eq!(sessions.count_agent_attribution("agent_b").unwrap(), 1);
    assert_eq!(sessions.count_agent_attribution("agent_a").unwrap(), 0);

    let (again_status, again_json) = rename(&state, "agent_a", "agent_b").await;
    assert_eq!(again_status, StatusCode::NOT_FOUND, "{again_json}");
    assert!(
        message_of(&again_json).contains("is not configured"),
        "{again_json}"
    );

    let (created_status, created_json) = create_agent(&state, "agent_a").await;
    assert!(created_status.is_success(), "{created_json}");
    assert_eq!(memory.count_agent("agent_a").await.unwrap(), 0);
    assert_eq!(memory.count_agent("agent_b").await.unwrap(), 1);
}

#[tokio::test]
async fn committed_rename_unrelated_target_stays_not_configured() {
    let tmp = tempfile::tempdir().unwrap();
    let config = fixture(tmp.path(), "agent_b");
    let state = test_state(config);
    let (status, json) = rename(&state, "agent_a", "agent_b").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{json}");
    assert!(message_of(&json).contains("is not configured"), "{json}");
}

#[tokio::test]
async fn committed_rename_rpc_unreadable_cron_is_not_alias_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = fixture(tmp.path(), "agent_a");
    commit_alias(&mut config, "agent_a", "agent_b");
    config.save().await.unwrap();
    std::fs::create_dir_all(config.data_dir.join("cron").join("jobs.db")).unwrap();
    let saved = config.clone();

    let messages = rpc_roundtrip(
        config,
        vec![(
            2,
            "config/map-key-rename".into(),
            serde_json::json!({"path": "agents", "from": "agent_a", "to": "agent_b"}),
        )],
    )
    .await;
    let rename_msg = rpc_by_id(&messages, 2);
    let err = rename_msg["error"]["message"].as_str().unwrap_or("");
    assert!(
        rename_msg.get("error").is_some(),
        "unreadable cron fails the rpc rename: {rename_msg}"
    );
    assert!(
        !err.contains("alias not found") && !err.contains("is not configured"),
        "rpc must not report the alias missing when cron cannot be read: {err}"
    );

    let create_messages = rpc_roundtrip(
        saved,
        vec![(
            2,
            "config/map-key-create".into(),
            serde_json::json!({"path": "agents", "key": "agent_a"}),
        )],
    )
    .await;
    let create_msg = rpc_by_id(&create_messages, 2);
    assert!(
        create_msg.get("error").is_some(),
        "rpc create of the retired alias stays refused: {create_msg}"
    );
}

#[tokio::test]
async fn committed_rename_rpc_resumes_workspace_then_reports_alias_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = fixture(tmp.path(), "agent_a");
    let old_ws = config.agent_workspace_dir("agent_a");
    std::fs::create_dir_all(&old_ws).unwrap();
    std::fs::write(old_ws.join("marker"), b"rpc").unwrap();
    let new_ws = {
        let mut preview = config.clone();
        commit_alias(&mut preview, "agent_a", "agent_b");
        preview.agent_workspace_dir("agent_b")
    };
    commit_alias(&mut config, "agent_a", "agent_b");
    config.save().await.unwrap();

    let messages = rpc_roundtrip(
        config.clone(),
        vec![(
            2,
            "config/map-key-rename".into(),
            serde_json::json!({"path": "agents", "from": "agent_a", "to": "agent_b"}),
        )],
    )
    .await;
    let first = rpc_by_id(&messages, 2);
    assert!(
        first.get("result").is_some(),
        "workspace residue resumes over rpc: {first}"
    );
    assert!(
        new_ws.join("marker").is_file(),
        "marker moved to the new workspace"
    );
    assert!(!old_ws.exists());

    let second_messages = rpc_roundtrip(
        config,
        vec![(
            2,
            "config/map-key-rename".into(),
            serde_json::json!({"path": "agents", "from": "agent_a", "to": "agent_b"}),
        )],
    )
    .await;
    let second = rpc_by_id(&second_messages, 2);
    let err = second["error"]["message"].as_str().unwrap_or("");
    assert!(
        err.contains("alias not found"),
        "a finished rename reports the alias missing on the next call: {second}"
    );
}

#[tokio::test]
async fn committed_rename_rpc_blocked_workspace_refuses_create() {
    let tmp = tempfile::tempdir().unwrap();
    let config = fixture(tmp.path(), "agent_a");
    let old_ws = config.agent_workspace_dir("agent_a");
    std::fs::create_dir_all(&old_ws).unwrap();
    let blocker = tmp.path().join("agents").join("agent_b");
    std::fs::create_dir_all(blocker.parent().unwrap()).unwrap();
    std::fs::write(&blocker, b"x").unwrap();
    config.save().await.unwrap();

    let messages = rpc_roundtrip(
        config.clone(),
        vec![
            (
                2,
                "config/map-key-rename".into(),
                serde_json::json!({"path": "agents", "from": "agent_a", "to": "agent_b"}),
            ),
            (
                3,
                "config/map-key-create".into(),
                serde_json::json!({"path": "agents", "key": "agent_a"}),
            ),
        ],
    )
    .await;
    let renamed = rpc_by_id(&messages, 2);
    assert!(
        renamed.get("result").is_some(),
        "blocked workspace still commits the rename: {renamed}"
    );
    let created = rpc_by_id(&messages, 3);
    assert!(
        created.get("error").is_some(),
        "rpc refuses to recreate the retired alias while recovery is open: {created}"
    );
}

#[tokio::test]
async fn deleting_the_target_of_an_unfinished_rename_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, old_ws, _blocker) = blocked_rename(tmp.path()).await;

    let (status, json) = delete_agent(&state, "agent_b").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    assert_eq!(json["code"], "validation_failed", "{json}");
    assert!(
        message_of(&json).contains("target of an unfinished rename"),
        "{json}"
    );
    assert!(
        state.config.read().agents.contains_key("agent_b"),
        "the refused delete leaves the target configured"
    );
    assert!(
        old_ws.join("marker").is_file(),
        "the state the rename still owes the target stays put"
    );

    let live = state.config.read().clone();
    let messages = rpc_roundtrip(
        live,
        vec![(
            2,
            "config/map-key-delete".into(),
            serde_json::json!({"path": "agents", "key": "agent_b"}),
        )],
    )
    .await;
    let deleted = rpc_by_id(&messages, 2);
    assert_eq!(
        deleted["error"]["code"],
        zeroclaw_api::jsonrpc::error_codes::INVALID_PARAMS,
        "rpc refuses the same delete: {deleted}"
    );
    assert!(
        deleted["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("target of an unfinished rename")),
        "rpc names the same refusal: {deleted}"
    );
}

#[tokio::test]
async fn deleting_a_retired_alias_re_added_by_hand_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, old_ws, _blocker) = blocked_rename(tmp.path()).await;
    // A hand edit brings the retired alias back around the create guards.
    state.config.write().agents.insert(
        "agent_a".to_string(),
        AliasedAgentConfig {
            risk_profile: "default".into(),
            ..AliasedAgentConfig::default()
        },
    );

    let (status, json) = delete_agent(&state, "agent_a").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    assert_eq!(json["code"], "validation_failed", "{json}");
    assert!(
        message_of(&json).contains("retired by an unfinished agent rename"),
        "{json}"
    );
    assert!(
        old_ws.join("marker").is_file(),
        "the delete cascade must not archive the workspace the rename still owes"
    );
    assert!(
        !tmp.path()
            .join("data")
            .join("agents")
            .join("_deleted")
            .exists(),
        "nothing was archived"
    );

    let live = state.config.read().clone();
    let messages = rpc_roundtrip(
        live,
        vec![(
            2,
            "config/map-key-delete".into(),
            serde_json::json!({"path": "agents", "key": "agent_a"}),
        )],
    )
    .await;
    let deleted = rpc_by_id(&messages, 2);
    assert_eq!(
        deleted["error"]["code"],
        zeroclaw_api::jsonrpc::error_codes::INVALID_PARAMS,
        "rpc refuses the same delete: {deleted}"
    );
    assert!(
        deleted["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("retired by an unfinished agent rename")),
        "rpc names the same refusal: {deleted}"
    );
}

#[tokio::test]
async fn refusals_of_a_retired_alias_never_name_the_pending_target() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, _old_ws, _blocker) = blocked_rename(tmp.path()).await;
    let (created_status, created_json) = create_agent(&state, "agent_c").await;
    assert!(created_status.is_success(), "{created_json}");

    // Each refusal is rendered before the caller's view of the pending rename
    // is known, so none may reveal where the retired alias's state is going.
    let hidden = "agent_b";
    let (status, json) = create_agent(&state, "agent_a").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    assert!(names_the_unfinished_rename(&message_of(&json)), "{json}");
    assert!(!json.to_string().contains(hidden), "create: {json}");
    let (status, json) = rename(&state, "agent_c", "agent_a").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    assert!(names_the_unfinished_rename(&message_of(&json)), "{json}");
    assert!(!json.to_string().contains(hidden), "rename onto: {json}");
    let (status, json) = rename(&state, "agent_a", "agent_d").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    assert!(names_the_unfinished_rename(&message_of(&json)), "{json}");
    assert!(!json.to_string().contains(hidden), "rename away: {json}");

    let live = state.config.read().clone();
    let messages = rpc_roundtrip(
        live,
        vec![
            (
                2,
                "config/map-key-create".into(),
                serde_json::json!({"path": "agents", "key": "agent_a"}),
            ),
            (
                3,
                "config/map-key-rename".into(),
                serde_json::json!({"path": "agents", "from": "agent_c", "to": "agent_a"}),
            ),
            (
                4,
                "config/map-key-rename".into(),
                serde_json::json!({"path": "agents", "from": "agent_a", "to": "agent_d"}),
            ),
        ],
    )
    .await;
    for id in [2, 3, 4] {
        let refused = rpc_by_id(&messages, id);
        assert_rpc_rename_refusal(&refused);
        assert!(!refused.to_string().contains(hidden), "rpc {id}: {refused}");
    }
}

#[tokio::test]
async fn a_rename_whose_old_alias_is_configured_again_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, old_ws, _blocker) = blocked_rename(tmp.path()).await;
    configure_again(&state, "agent_a");

    let (status, json) = rename(&state, "agent_a", "agent_b").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    assert_eq!(json["code"], "validation_failed", "{json}");
    assert!(message_of(&json).contains("configured again"), "{json}");
    assert!(
        old_ws.join("marker").is_file(),
        "nothing moves while the old alias is configured again"
    );
    {
        let live = state.config.read();
        assert!(live.agents.contains_key("agent_a"));
        assert!(live.agents.contains_key("agent_b"));
    }

    let live = state.config.read().clone();
    let messages = rpc_roundtrip(
        live,
        vec![(
            2,
            "config/map-key-rename".into(),
            serde_json::json!({"path": "agents", "from": "agent_a", "to": "agent_b"}),
        )],
    )
    .await;
    let refused = rpc_by_id(&messages, 2);
    assert_eq!(
        refused["error"]["code"],
        zeroclaw_api::jsonrpc::error_codes::INVALID_PARAMS,
        "{refused}"
    );
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("configured again")),
        "{refused}"
    );
    assert!(old_ws.join("marker").is_file());
}

/// An unfinished rename of `agent_a` to `agent_b` with `agent_a` configured
/// again by hand, next to `agent_c`, which no rename owns.
async fn previewed_rename(dir: &std::path::Path) -> crate::AppState {
    let (state, _old_ws, _blocker) = blocked_rename(dir).await;
    configure_again(&state, "agent_a");
    let (created_status, created_json) = create_agent(&state, "agent_c").await;
    assert!(created_status.is_success(), "{created_json}");
    state
}

#[tokio::test]
async fn the_delete_plan_reports_the_refusal_the_delete_makes() {
    let tmp = tempfile::tempdir().unwrap();
    let state = previewed_rename(tmp.path()).await;

    // The pending target and the retired alias configured again are both
    // refused by the delete, so the plan reports each as blocked.
    for alias in ["agent_b", "agent_a"] {
        let (status, json) = delete_plan(&state, alias).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["allowed"], false, "{alias}: {json}");
        let blockers = json["blockers"].as_array().cloned().unwrap_or_default();
        assert!(
            blockers.iter().any(|blocker| blocker["raw_value"]
                .as_str()
                .is_some_and(names_the_unfinished_rename)),
            "{alias}: the plan names the refusal: {json}"
        );
    }
    // An agent no unfinished rename owns stays deletable.
    let (status, json) = delete_plan(&state, "agent_c").await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["allowed"], true, "{json}");
}

#[tokio::test]
async fn the_rpc_delete_preview_reports_the_refusal_the_delete_makes() {
    let tmp = tempfile::tempdir().unwrap();
    let state = previewed_rename(tmp.path()).await;

    // The daemon's preview blocks the pending target and the retired alias
    // configured again, and still allows the agent no rename owns.
    let live = state.config.read().clone();
    let messages = rpc_roundtrip(
        live,
        vec![
            (
                2,
                "agents/delete-preview".into(),
                serde_json::json!({"alias": "agent_b"}),
            ),
            (
                3,
                "agents/delete-preview".into(),
                serde_json::json!({"alias": "agent_a"}),
            ),
            (
                4,
                "agents/delete-preview".into(),
                serde_json::json!({"alias": "agent_c"}),
            ),
        ],
    )
    .await;
    for id in [2, 3] {
        let preview = rpc_by_id(&messages, id);
        assert_eq!(preview["result"]["allowed"], false, "{preview}");
        let blockers = preview["result"]["blockers"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            blockers
                .iter()
                .any(|blocker| blocker.as_str().is_some_and(names_the_unfinished_rename)),
            "the rpc preview names the refusal: {preview}"
        );
    }
    let preview = rpc_by_id(&messages, 4);
    assert_eq!(preview["result"]["allowed"], true, "{preview}");
}
