//! The config writes, Quickstart and the admin reload, served through the
//! core, answer exactly what the in-process routes answer and leave the same
//! configuration behind.
//!
//! A write changes what it reads, so each case builds two identical sides
//! from one seed: an in-process gateway state, and a core reached over the
//! daemon's real in-process connector, each with its own copy of the same
//! `config.toml`. The in-process route runs on one, the core-backed body (as
//! `zeroclaw-gw` serves it, its refusals rendered through `explain`) on the
//! other. The answers must be equal in status, content type and body, and
//! the live configuration and the saved file must match afterwards.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use zeroclaw_config::schema::Config;
use zeroclaw_runtime::rpc::context::RpcContext;
use zeroclaw_runtime::rpc::inproc::InprocConnector;

use crate::AppState;
use crate::api_config::{MapKeyQuery, PropPutBody, PropQuery, RenameMapKeyBody};
use crate::core_rpc::{CoreAccess, CoreCall, CoreError, CoreRpc};
use crate::refusal_parity::{Answer, answer};

const TOKEN: &str = "zc_config_write_operator";

/// One side of a case: its directory, holding its `config.toml`.
struct Side {
    dir: tempfile::TempDir,
}

impl Side {
    async fn new(seed: &dyn Fn(&mut Config)) -> (Self, Config) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = Config {
            data_dir: dir.path().to_path_buf(),
            config_path: dir.path().join("config.toml"),
            ..Default::default()
        };
        config.gateway.require_pairing = true;
        config.gateway.paired_tokens = vec![TOKEN.into()];
        seed(&mut config);
        config.save().await.expect("save the seed config");
        (Self { dir }, config)
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The saved file, with this side's directory replaced and each
    /// encrypted secret masked (each side encrypts under its own key, with a
    /// fresh nonce), so the two sides compare.
    fn saved(&self) -> String {
        let text = std::fs::read_to_string(self.path().join("config.toml"))
            .expect("the saved config")
            .replace(&self.path().display().to_string(), "<dir>");
        let mut masked = String::with_capacity(text.len());
        let mut rest = text.as_str();
        while let Some(at) = rest.find("enc2:") {
            masked.push_str(&rest[..at + "enc2:".len()]);
            rest = &rest[at + "enc2:".len()..];
            let ciphertext = rest.bytes().take_while(u8::is_ascii_hexdigit).count();
            masked.push_str("<ciphertext>");
            rest = &rest[ciphertext..];
        }
        masked.push_str(rest);
        masked
    }
}

/// Two identical sides: the in-process gateway's state, and a core.
struct Pair {
    state: AppState,
    in_process: Side,
    ctx: Arc<RpcContext>,
    core: CoreRpc,
    core_side: Side,
    cancel: tokio_util::sync::CancellationToken,
}

impl Drop for Pair {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Pair {
    async fn new(seed: impl Fn(&mut Config)) -> Self {
        let (in_process, config) = Side::new(&seed).await;
        let state = crate::api::test_state(config);
        let (core_side, config) = Side::new(&seed).await;
        let sessions = Arc::new(zeroclaw_runtime::rpc::session::SessionStore::new(
            16,
            Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
                4, 10, 60,
            )),
        ));
        let ctx = RpcContext::for_live_test(config, sessions);
        let cancel = tokio_util::sync::CancellationToken::new();
        let connector = InprocConnector::new(cancel.clone());
        connector.bind(Arc::clone(&ctx));
        Self {
            state,
            in_process,
            ctx,
            core: CoreRpc::inproc(connector, || true),
            core_side,
            cancel,
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

    async fn call(&self) -> CoreCall {
        match self.core.access(&Self::headers()).await {
            Ok(CoreAccess::Core(call)) => call,
            Ok(CoreAccess::InProcess) => panic!("served in-process"),
            Err(error) => panic!("no core access: {error:?}"),
        }
    }

    /// Both sides now hold the same value at each of `paths`, live and on
    /// disk, and the same saved file.
    fn assert_same_config(&self, case: &str, paths: &[&str]) {
        for path in paths {
            let in_process = self.state.config.read().get_prop(path).ok();
            let core = self.ctx.config.read().get_prop(path).ok();
            assert_eq!(in_process, core, "{case}: live {path}");
        }
        // Incremental saves may insert equal tables in a different order.
        // TOML values retain array ordering; comments are pinned by the cases
        // that write them below rather than treated as configuration values.
        let in_process: toml::Value = toml::from_str(&self.in_process.saved()).expect("saved TOML");
        let through_core: toml::Value =
            toml::from_str(&self.core_side.saved()).expect("saved TOML");
        assert_eq!(
            in_process, through_core,
            "{case}: the saved config.toml values"
        );
    }
}

/// The core-backed answer as `zeroclaw-gw` serves it.
fn served(result: Result<Response, CoreError>) -> Response {
    result.unwrap_or_else(crate::preview::explain)
}

/// Require `case` to answer `expected` on both sides, with equal answers.
async fn assert_parity(
    case: &str,
    expected: StatusCode,
    in_process: Response,
    through_core: Response,
) -> Answer {
    let in_process = answer(in_process).await;
    let through_core = answer(through_core).await;
    assert_eq!(in_process.status, expected, "{case}: {in_process:?}");
    assert_eq!(through_core, in_process, "{case}");
    in_process
}

// ── PATCH /api/config ────────────────────────────────────────────

async fn patch_both(pair: &Pair, headers: HeaderMap, body: Value) -> (Response, Response) {
    let in_process = crate::api_config::handle_patch(
        State(pair.state.clone()),
        None,
        headers.clone(),
        Json(body.clone()),
    )
    .await;
    let call = pair.call().await;
    let through_core = served(crate::api_config::patch_through_core(&call, &headers, body).await);
    (in_process, through_core)
}

#[tokio::test]
async fn a_patch_answers_and_saves_alike() {
    let pair = Pair::new(|_| {}).await;
    let body = json!([
        {"op": "replace", "path": "/memory/auto_save", "value": false, "comment": "kept small"},
        {"op": "test", "path": "memory.auto_save", "value": false},
        {"op": "add", "path": "/scheduler/max_run_history", "value": 7},
        {"op": "remove", "path": "/memory/backend"},
        {"op": "comment", "path": "scheduler.max_run_history", "comment": "why"},
    ]);
    let (in_process, through_core) = patch_both(&pair, HeaderMap::new(), body).await;
    let answered = assert_parity("a mixed patch", StatusCode::OK, in_process, through_core).await;
    let crate::refusal_parity::AnswerBody::Json(body) = answered.body else {
        panic!("a JSON body");
    };
    assert_eq!(body["results"][3]["value"], Value::Null, "{body}");
    assert!(body["results"][3].get("value").is_some(), "{body}");
    pair.assert_same_config(
        "a mixed patch",
        &[
            "memory.backend",
            "memory.auto_save",
            "scheduler.max_run_history",
        ],
    );
    for comment in ["kept small", "why"] {
        assert!(
            pair.in_process.saved().contains(comment),
            "in-process comment is written"
        );
        assert!(
            pair.core_side.saved().contains(comment),
            "core comment is written"
        );
    }
}

#[tokio::test]
async fn an_empty_patch_answers_alike() {
    let pair = Pair::new(|_| {}).await;
    let (in_process, through_core) = patch_both(&pair, HeaderMap::new(), json!([])).await;
    assert_parity("an empty patch", StatusCode::OK, in_process, through_core).await;
}

#[tokio::test]
async fn a_refused_patch_answers_alike_and_saves_nothing() {
    for (case, expected, body) in [
        (
            "an unsupported op",
            StatusCode::BAD_REQUEST,
            json!([{"op": "move", "path": "/memory/backend"}]),
        ),
        (
            "an unknown op, second",
            StatusCode::BAD_REQUEST,
            json!([
                {"op": "replace", "path": "/memory/backend", "value": "none"},
                {"op": "frobnicate", "path": "/memory/backend"},
            ]),
        ),
        (
            "a path the schema does not define",
            StatusCode::NOT_FOUND,
            json!([{"op": "replace", "path": "/memory/no_such_field", "value": "x"}]),
        ),
        (
            "an integer cannot be cleared with an empty string",
            StatusCode::BAD_REQUEST,
            json!([
                {"op": "replace", "path": "/scheduler/max_run_history", "value": 7},
                {"op": "remove", "path": "/scheduler/max_run_history"},
            ]),
        ),
        (
            "a value of the wrong kind",
            StatusCode::BAD_REQUEST,
            json!([{"op": "replace", "path": "/scheduler/max_run_history", "value": "many"}]),
        ),
        (
            "a failed test",
            StatusCode::BAD_REQUEST,
            json!([{"op": "test", "path": "/memory/backend", "value": "not-this"}]),
        ),
        (
            "a test of a secret",
            StatusCode::BAD_REQUEST,
            json!([{"op": "test", "path": "/composio/api_key", "value": "x"}]),
        ),
        (
            "a masked secret",
            StatusCode::BAD_REQUEST,
            json!([{"op": "replace", "path": "/composio/api_key", "value": "****"}]),
        ),
        (
            "a body that is not a list",
            StatusCode::BAD_REQUEST,
            json!({"op": "replace"}),
        ),
    ] {
        let pair = Pair::new(|_| {}).await;
        let (in_process, through_core) = patch_both(&pair, HeaderMap::new(), body).await;
        assert_parity(case, expected, in_process, through_core).await;
        pair.assert_same_config(case, &["memory.backend"]);
    }
}

#[tokio::test]
async fn a_patch_of_a_drifted_path_is_refused_unless_overridden() {
    let edit_outside = |side: &Side| {
        let path = side.path().join("config.toml");
        let mut saved: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap())
            .expect("the saved config parses");
        saved
            .entry("memory")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .expect("a memory table")
            .insert("backend".into(), toml::Value::String("markdown".into()));
        std::fs::write(&path, toml::to_string(&saved).unwrap()).unwrap();
    };
    let body = json!([{"op": "replace", "path": "/memory/backend", "value": "none"}]);

    let pair = Pair::new(|_| {}).await;
    edit_outside(&pair.in_process);
    edit_outside(&pair.core_side);
    let (in_process, through_core) = patch_both(&pair, HeaderMap::new(), body.clone()).await;
    assert_parity(
        "a drifted path",
        StatusCode::CONFLICT,
        in_process,
        through_core,
    )
    .await;

    let mut overriding = HeaderMap::new();
    overriding.insert(
        "x-zeroclaw-override-drift",
        HeaderValue::from_static("true"),
    );
    let (in_process, through_core) = patch_both(&pair, overriding, body).await;
    assert_parity(
        "a drifted path, overridden",
        StatusCode::OK,
        in_process,
        through_core,
    )
    .await;
    pair.assert_same_config("a drifted path, overridden", &["memory.backend"]);
}

// ── PUT and DELETE /api/config/prop ──────────────────────────────

async fn put_both(pair: &Pair, body: Value) -> (Response, Response) {
    let parse = || serde_json::from_value::<PropPutBody>(body.clone()).expect("a PUT body");
    let in_process =
        crate::api_config::handle_prop_put(State(pair.state.clone()), None, Json(parse())).await;
    let call = pair.call().await;
    let through_core = served(crate::api_config::prop_put_through_core(&call, parse()).await);
    (in_process, through_core)
}

async fn delete_both(pair: &Pair, path: &str) -> (Response, Response) {
    let query = || PropQuery { path: path.into() };
    let in_process =
        crate::api_config::handle_prop_delete(State(pair.state.clone()), None, Query(query()))
            .await;
    let call = pair.call().await;
    let through_core = served(crate::api_config::prop_delete_through_core(&call, query()).await);
    (in_process, through_core)
}

#[tokio::test]
async fn a_property_write_answers_and_saves_alike() {
    for (case, expected, body, paths) in [
        (
            "a value",
            StatusCode::OK,
            json!({"path": "memory.backend", "value": "none", "comment": "small"}),
            &["memory.backend"][..],
        ),
        (
            "a typed value",
            StatusCode::OK,
            json!({"path": "scheduler.max_run_history", "value": 9}),
            &["scheduler.max_run_history"][..],
        ),
        (
            "a secret",
            StatusCode::OK,
            json!({"path": "composio.api_key", "value": "sk-parity"}),
            &[][..],
        ),
        (
            "a path the schema does not define",
            StatusCode::NOT_FOUND,
            json!({"path": "memory.no_such_field", "value": "x"}),
            &[][..],
        ),
        (
            "a JSON Pointer",
            StatusCode::NOT_FOUND,
            json!({"path": "/memory/backend", "value": "none"}),
            &["memory.backend"][..],
        ),
        (
            "a masked secret",
            StatusCode::BAD_REQUEST,
            json!({"path": "composio.api_key", "value": "****"}),
            &[][..],
        ),
    ] {
        let pair = Pair::new(|_| {}).await;
        let (in_process, through_core) = put_both(&pair, body).await;
        assert_parity(case, expected, in_process, through_core).await;
        pair.assert_same_config(case, paths);
    }
}

#[tokio::test]
async fn a_property_delete_answers_and_saves_alike() {
    let seed = |config: &mut Config| {
        config.memory.backend = "none".into();
        config.composio.api_key = Some("sk-seeded".into());
    };
    for (case, expected, path) in [
        ("a value", StatusCode::OK, "memory.backend"),
        ("a secret", StatusCode::OK, "composio.api_key"),
        (
            "a path the schema does not define",
            StatusCode::NOT_FOUND,
            "memory.no_such_field",
        ),
        ("a JSON Pointer", StatusCode::NOT_FOUND, "/memory/backend"),
    ] {
        let pair = Pair::new(seed).await;
        let (in_process, through_core) = delete_both(&pair, path).await;
        assert_parity(case, expected, in_process, through_core).await;
        pair.assert_same_config(case, &["memory.backend"]);
    }
}

// ── Map keys ─────────────────────────────────────────────────────

#[tokio::test]
async fn a_map_key_create_answers_saves_and_scaffolds_alike() {
    for (case, expected, path, key) in [
        ("a skill bundle", StatusCode::OK, "skill_bundles", "extra"),
        ("an agent", StatusCode::OK, "agents", "helper"),
        (
            "an unknown section",
            StatusCode::NOT_FOUND,
            "no_such_section",
            "x",
        ),
        (
            "the reserved agent",
            StatusCode::BAD_REQUEST,
            "agents",
            "default",
        ),
    ] {
        let pair = Pair::new(|_| {}).await;
        let query = || MapKeyQuery {
            path: path.into(),
            key: key.into(),
        };
        let in_process =
            crate::api_config::handle_map_key(State(pair.state.clone()), None, Query(query()))
                .await;
        let call = pair.call().await;
        let through_core =
            served(crate::api_config::map_key_create_through_core(&call, query()).await);
        assert_parity(case, expected, in_process, through_core).await;
        pair.assert_same_config(case, &[]);
        if path == "skill_bundles" {
            for (side, config) in [
                ("in-process", pair.state.config.read().clone()),
                ("core", pair.ctx.config.read().clone()),
            ] {
                let dir = zeroclaw_config::skill_bundles::resolve_directory(
                    &config,
                    &config.install_root_dir(),
                    key,
                )
                .expect("the bundle's directory");
                assert!(dir.is_dir(), "{case}: {side} created {}", dir.display());
            }
        }
    }
}

#[tokio::test]
async fn a_map_key_rename_answers_and_saves_alike() {
    let seed = |config: &mut Config| {
        config
            .create_map_key("skill_bundles", "old")
            .expect("seed a bundle");
    };
    for (case, expected, path, from, to) in [
        ("a rename", StatusCode::OK, "skill_bundles", "old", "new"),
        (
            "a missing generic key",
            StatusCode::OK,
            "skill_bundles",
            "absent",
            "other",
        ),
        (
            "a missing cascade alias",
            StatusCode::NOT_FOUND,
            "providers.models.openai",
            "absent",
            "other",
        ),
    ] {
        let pair = Pair::new(seed).await;
        let body = || RenameMapKeyBody {
            path: path.into(),
            from: from.into(),
            to: to.into(),
        };
        let in_process =
            crate::api_config::handle_rename_map_key(State(pair.state.clone()), None, Json(body()))
                .await;
        let call = pair.call().await;
        let through_core =
            served(crate::api_config::rename_map_key_through_core(&call, body()).await);
        assert_parity(case, expected, in_process, through_core).await;
        pair.assert_same_config(case, &[]);
    }
}

// ── Quickstart ───────────────────────────────────────────────────

#[tokio::test]
async fn the_quickstart_reads_answer_alike() {
    use crate::api_quickstart::{FieldsRequest, handle_fields, handle_state};
    use zeroclaw_runtime::quickstart::FieldSection;

    let pair = Pair::new(|_| {}).await;
    let call = pair.call().await;
    assert_parity(
        "the state",
        StatusCode::OK,
        handle_state(State(pair.state.clone()))
            .await
            .into_response(),
        served(crate::api_quickstart::state_through_core(&call).await),
    )
    .await;
    let fields = || FieldsRequest {
        section: FieldSection::ModelProvider,
        type_key: "anthropic".into(),
    };
    assert_parity(
        "the fields",
        StatusCode::OK,
        handle_fields(State(pair.state.clone()), Json(fields()))
            .await
            .into_response(),
        served(crate::api_quickstart::fields_through_core(&call, fields()).await),
    )
    .await;
}

fn submission(agent: &str) -> zeroclaw_config::presets::BuilderSubmission {
    use zeroclaw_config::presets::{
        AgentIdentity, BuilderSubmission, MemoryChoice, ModelProviderChoice, SelectorChoice,
    };
    BuilderSubmission {
        model_provider: SelectorChoice::Fresh(ModelProviderChoice {
            provider_type: "anthropic".into(),
            alias: "anthropic".into(),
            model: "claude-sonnet-4-5".into(),
            fields: std::collections::HashMap::from([("api_key".into(), "sk-test".into())]),
        }),
        risk_profile: SelectorChoice::Fresh("balanced".into()),
        runtime_profile: SelectorChoice::Fresh("balanced".into()),
        memory: SelectorChoice::Fresh(MemoryChoice::Sqlite),
        channels: vec![],
        peer_groups: vec![],
        agent: AgentIdentity {
            name: agent.into(),
            system_prompt: "You are helpful.".into(),
            personality_file: None,
            personality_files: vec![],
        },
    }
}

#[tokio::test]
async fn a_quickstart_submission_validates_and_applies_alike() {
    use crate::api_quickstart::{handle_apply, handle_validate};

    for (case, agent) in [("a valid submission", "assistant"), ("an invalid name", "")] {
        let pair = Pair::new(|_| {}).await;
        let call = pair.call().await;
        assert_parity(
            &format!("{case}, validated"),
            StatusCode::OK,
            handle_validate(State(pair.state.clone()), Json(submission(agent)))
                .await
                .into_response(),
            served(crate::api_quickstart::validate_through_core(&call, submission(agent)).await),
        )
        .await;
        assert_parity(
            &format!("{case}, applied"),
            StatusCode::OK,
            handle_apply(State(pair.state.clone()), None, Json(submission(agent))).await,
            served(crate::api_quickstart::apply_through_core(&call, submission(agent)).await),
        )
        .await;
        pair.assert_same_config(case, &["agents.assistant.model_provider"]);
    }
}

#[tokio::test]
async fn a_quickstart_dismissal_answers_alike() {
    use crate::api_quickstart::{DismissRequest, handle_dismiss};
    use zeroclaw_runtime::quickstart::Surface;

    let pair = Pair::new(|_| {}).await;
    let call = pair.call().await;
    let request = || DismissRequest {
        run_id: "run-parity".into(),
        surface: Surface::Web,
        last_step: None,
    };
    assert_parity(
        "a dismissal",
        StatusCode::NO_CONTENT,
        handle_dismiss(State(pair.state.clone()), Json(request()))
            .await
            .into_response(),
        served(crate::api_quickstart::dismiss_through_core(&call, request()).await),
    )
    .await;
}

// ── POST /admin/reload ───────────────────────────────────────────

#[tokio::test]
async fn an_admin_reload_answers_alike_where_no_supervisor_can_reload() {
    for (case, expected, peer) in [
        (
            "from loopback",
            StatusCode::SERVICE_UNAVAILABLE,
            "127.0.0.1:50000",
        ),
        (
            "from elsewhere, not opted in",
            StatusCode::FORBIDDEN,
            "192.0.2.7:50000",
        ),
    ] {
        let pair = Pair::new(|_| {}).await;
        let peer: SocketAddr = peer.parse().unwrap();
        let in_process = crate::handle_admin_reload(
            State(pair.state.clone()),
            ConnectInfo(peer),
            Pair::headers(),
        )
        .await
        .into_response();
        let call = pair.call().await;
        let through_core =
            served(crate::admin_reload_through_core(&call, peer.ip().is_loopback()).await);
        assert_parity(case, expected, in_process, through_core).await;
    }
}
