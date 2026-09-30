//! Dispatcher-level tests for the core-parity methods: routing, the coarse
//! `Method::authz()` gate, and each method's selector. Body parity with the
//! HTTP routes is covered by the gateway's `p6_parity_tests`.

use super::*;

/// Principals bound by peer uid: `scoped` may use agent `alpha` with every
/// `files` verb, `ungranted` may use `alpha` and read sessions but holds no
/// `files` grant, and `wildcard` may use every agent with every `files` verb
/// but is not admin. Agents `alpha` and `beta` are configured, since a profile
/// naming only unconfigured agents grants nothing.
const SCOPED: u32 = 5101;
const UNGRANTED: u32 = 5102;
const WILDCARD_UID: u32 = 5103;

fn files_config(tmp: &tempfile::TempDir) -> zeroclaw_config::schema::Config {
    use std::collections::HashMap;
    use zeroclaw_api::grants::{Resource, Verb};
    use zeroclaw_config::schema::{AliasedAgentConfig, PermissionProfileConfig, UserConfig};

    let mut config = zeroclaw_config::schema::Config {
        config_path: tmp.path().join("config.toml"),
        ..zeroclaw_config::schema::Config::default()
    };
    for alias in ["alpha", "beta"] {
        config.agents.insert(
            alias.into(),
            AliasedAgentConfig {
                enabled: true,
                ..Default::default()
            },
        );
    }
    let all_files = HashMap::from([(
        Resource::Files,
        vec![Verb::Create, Verb::Read, Verb::Update, Verb::Delete],
    )]);
    for (name, agents, grants) in [
        ("files-alpha", vec!["alpha"], all_files.clone()),
        (
            "alpha-only",
            vec!["alpha"],
            HashMap::from([(Resource::Sessions, vec![Verb::Read])]),
        ),
        (
            "files-everyone",
            vec![zeroclaw_api::grants::WILDCARD],
            all_files,
        ),
    ] {
        config.permission_profiles.insert(
            name.into(),
            PermissionProfileConfig {
                allowed_agents: agents.into_iter().map(str::to_string).collect(),
                grants,
                ..PermissionProfileConfig::default()
            },
        );
    }
    for (user, uid, profile) in [
        ("scoped", SCOPED, "files-alpha"),
        ("ungranted", UNGRANTED, "alpha-only"),
        ("wildcard", WILDCARD_UID, "files-everyone"),
    ] {
        config.users.insert(
            user.into(),
            UserConfig {
                uid: Some(uid),
                permission_profiles: vec![profile.into()],
                ..UserConfig::default()
            },
        );
    }
    let alpha = tmp.path().join("agents/alpha/workspace");
    std::fs::create_dir_all(alpha.join("notes")).unwrap();
    std::fs::write(alpha.join("notes/todo.md"), b"todo").unwrap();
    let beta = tmp.path().join("agents/beta/workspace");
    std::fs::create_dir_all(&beta).unwrap();
    std::fs::write(beta.join("secret.md"), b"beta only").unwrap();
    std::fs::create_dir_all(tmp.path().join("shared")).unwrap();
    config
}

fn assert_forbidden(response: &Value, what: &str) {
    assert_eq!(
        response["error"]["code"],
        json!(FORBIDDEN),
        "{what}: {response}"
    );
}

#[tokio::test]
async fn a_scoped_principal_works_in_its_own_agent_workspace() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(files_config(&tmp));
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;

    let listed = rpc(
        &mut peer,
        &mut rx,
        1,
        "workspace/list",
        json!({"agent": "alpha"}),
    )
    .await;
    assert_eq!(
        listed["result"]["entries"][0]["name"],
        json!("notes"),
        "{listed}"
    );
    let read = rpc(
        &mut peer,
        &mut rx,
        2,
        "fs/read",
        json!({"agent": "alpha", "path": "notes/todo.md"}),
    )
    .await;
    assert_eq!(read["result"]["content"], json!("todo"), "{read}");
    let made = rpc(
        &mut peer,
        &mut rx,
        3,
        "fs/mkdir",
        json!({"agent": "alpha", "path": "d"}),
    )
    .await;
    assert_eq!(made["result"], json!({"created": "d"}), "{made}");
    let moved = rpc(
        &mut peer,
        &mut rx,
        4,
        "fs/move",
        json!({"agent": "alpha", "from": "notes/todo.md", "to": "d/todo.md"}),
    )
    .await;
    assert_eq!(
        moved["result"],
        json!({"from": "notes/todo.md", "to": "d/todo.md"}),
        "{moved}"
    );
    let deleted = rpc(
        &mut peer,
        &mut rx,
        5,
        "fs/delete",
        json!({"agent": "alpha", "path": "d/todo.md"}),
    )
    .await;
    assert_eq!(
        deleted["result"],
        json!({"removed": "d/todo.md"}),
        "{deleted}"
    );
    assert!(!tmp.path().join("agents/alpha/workspace/d/todo.md").exists());
}

#[tokio::test]
async fn a_scoped_principal_cannot_reach_another_agents_workspace() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(files_config(&tmp));
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;

    let mut messages = Vec::new();
    for (id, method, params) in [
        (1, "workspace/list", json!({"agent": "beta"})),
        (2, "fs/read", json!({"agent": "beta", "path": "secret.md"})),
        (3, "fs/read", json!({"agent": "beta", "path": "absent.md"})),
        (
            4,
            "fs/delete",
            json!({"agent": "beta", "path": "secret.md"}),
        ),
        (
            5,
            "fs/move",
            json!({"agent": "beta", "from": "secret.md", "to": "x.md"}),
        ),
        (6, "fs/mkdir", json!({"agent": "beta", "path": "d"})),
    ] {
        let response = rpc(&mut peer, &mut rx, id, method, params).await;
        assert_forbidden(&response, method);
        messages.push((method, response["error"]["message"].clone()));
    }
    assert_eq!(
        messages[1].1, messages[2].1,
        "an existing and an absent file refuse alike, so the refusal reveals nothing"
    );
    assert!(tmp.path().join("agents/beta/workspace/secret.md").exists());
    assert!(!tmp.path().join("agents/beta/workspace/d").exists());
}

#[tokio::test]
async fn the_shared_area_needs_access_to_every_agent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(files_config(&tmp));
    let (mut scoped, mut scoped_rx) = roster_peer(&ctx, SCOPED).await;
    for (id, method, params) in [
        (1, "workspace/list", json!({})),
        (2, "fs/mkdir", json!({"path": "made"})),
        (3, "fs/rmdir", json!({"path": "made"})),
    ] {
        let response = rpc(&mut scoped, &mut scoped_rx, id, method, params).await;
        assert_forbidden(&response, method);
    }
    assert!(!tmp.path().join("shared/made").exists());

    let (mut wildcard, mut wildcard_rx) = roster_peer(&ctx, WILDCARD_UID).await;
    let made = rpc(
        &mut wildcard,
        &mut wildcard_rx,
        4,
        "fs/mkdir",
        json!({"path": "made"}),
    )
    .await;
    assert_eq!(made["result"], json!({"created": "made"}), "{made}");
    let removed = rpc(
        &mut wildcard,
        &mut wildcard_rx,
        5,
        "fs/rmdir",
        json!({"path": "made"}),
    )
    .await;
    assert_eq!(removed["result"], json!({"removed": "made"}), "{removed}");
}

#[tokio::test]
async fn workspace_methods_require_the_files_grant() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(files_config(&tmp));
    let (mut peer, mut rx) = roster_peer(&ctx, UNGRANTED).await;
    for (id, method, params) in [
        (1, "workspace/list", json!({"agent": "alpha"})),
        (
            2,
            "fs/read",
            json!({"agent": "alpha", "path": "notes/todo.md"}),
        ),
        (3, "fs/mkdir", json!({"agent": "alpha", "path": "d"})),
        (
            4,
            "fs/move",
            json!({"agent": "alpha", "from": "notes", "to": "n"}),
        ),
        (5, "fs/delete", json!({"agent": "alpha", "path": "notes"})),
        (6, "fs/rmdir", json!({"path": "made"})),
    ] {
        let response = rpc(&mut peer, &mut rx, id, method, params).await;
        assert_forbidden(&response, method);
    }
    assert!(tmp.path().join("agents/alpha/workspace/notes").exists());
}

/// An alias that escapes `<install>/agents/` names no configured agent, so a
/// wildcard principal is refused it at the selector; an operator, whose
/// grants cover unconfigured aliases, is refused it by the browse layer's
/// own alias check. Neither reaches the directory it points at.
#[tokio::test]
async fn an_alias_cannot_escape_the_agents_tree() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(files_config(&tmp));
    let outside = tmp.path().join("elsewhere");
    std::fs::create_dir_all(outside.join("workspace")).unwrap();
    std::fs::write(outside.join("workspace/secret.md"), b"outside").unwrap();
    let (mut peer, mut rx) = roster_peer(&ctx, WILDCARD_UID).await;
    let (mut operator, mut operator_rx) = local_operator(&ctx).await;

    let absolute = outside.to_string_lossy().to_string();
    for (id, agent) in [(1, "../elsewhere"), (2, absolute.as_str()), (3, "..")] {
        let params = json!({"agent": agent, "path": "secret.md"});
        let response = rpc(&mut peer, &mut rx, id, "fs/read", params.clone()).await;
        assert_forbidden(&response, &format!("alias {agent:?}"));
        let response = rpc(&mut operator, &mut operator_rx, id, "fs/read", params).await;
        assert_eq!(
            response["error"]["code"],
            json!(INVALID_PARAMS),
            "operator, alias {agent:?}: {response}"
        );
    }
    assert!(outside.join("workspace/secret.md").exists());
}

/// A wildcard selector covers every configured agent, not every string. An
/// alias the configuration does not define still names a directory under
/// the agents tree, a removed agent's workspace or a new one for any name,
/// so a principal without operator grants is refused it before anything
/// touches the disk.
#[tokio::test]
async fn a_wildcard_principal_reaches_only_configured_agents() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(files_config(&tmp));
    let retired = tmp.path().join("agents/retired/workspace");
    std::fs::create_dir_all(&retired).unwrap();
    std::fs::write(retired.join("secret.md"), b"retired secret").unwrap();
    let (mut peer, mut rx) = roster_peer(&ctx, WILDCARD_UID).await;

    for (id, method, params) in [
        (
            1,
            "fs/read",
            json!({"agent": "retired", "path": "secret.md"}),
        ),
        (
            2,
            "fs/delete",
            json!({"agent": "retired", "path": "secret.md"}),
        ),
        (
            3,
            "fs/move",
            json!({"agent": "retired", "from": "secret.md", "to": "moved.md"}),
        ),
        (4, "workspace/list", json!({"agent": "retired"})),
        (5, "fs/mkdir", json!({"agent": "ghost", "path": "planted"})),
    ] {
        let response = rpc(&mut peer, &mut rx, id, method, params).await;
        assert_forbidden(&response, method);
    }
    assert!(retired.join("secret.md").is_file());
    assert!(!tmp.path().join("agents/ghost").exists());

    let read = rpc(
        &mut peer,
        &mut rx,
        6,
        "fs/read",
        json!({"agent": "beta", "path": "secret.md"}),
    )
    .await;
    assert_eq!(read["result"]["content"], json!("beta only"), "{read}");
}

/// Send one workspace request from `peer`, park its blocking worker after
/// admission and before it acts, run `change` there, then release the worker
/// and return the response. Reaching the pause proves the request was
/// admitted ahead of `change`.
async fn workspace_request_parked_on_its_worker(
    peer: &mut RpcDispatcher,
    rx: &mut tokio::sync::mpsc::Receiver<String>,
    ctx: &Arc<RpcContext>,
    id: u64,
    method: &str,
    params: Value,
    change: impl FnOnce(),
) -> Value {
    let (arrived, release) = ctx.sessions.set_test_workspace_worker_pause();
    let parked = std::sync::atomic::AtomicBool::new(false);
    let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
    let change_while_parked = async {
        arrived.notified().await;
        parked.store(true, std::sync::atomic::Ordering::SeqCst);
        change();
        release
            .send(())
            .expect("the parked worker waits for its release");
    };
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        tokio::join!(peer.process_line(&line), change_while_parked)
    })
    .await
    .expect("the request reaches its worker and completes");
    assert!(
        parked.load(std::sync::atomic::Ordering::SeqCst),
        "the change must land while the operation waits, or the test is vacuous"
    );
    response_for(rx, id).await
}

/// `files_config` with the `files-alpha` profile narrowed by `narrow`.
fn files_alpha_narrowed(
    ctx: &Arc<RpcContext>,
    narrow: impl FnOnce(&mut zeroclaw_config::schema::PermissionProfileConfig),
) -> zeroclaw_config::schema::Config {
    let mut config = ctx.config.read().clone();
    narrow(
        config
            .permission_profiles
            .get_mut("files-alpha")
            .expect("files_config defines files-alpha"),
    );
    config
}

/// A workspace operation's authority is judged again on its blocking worker,
/// where it takes effect, not only when it is admitted. An operation admitted
/// and then waiting for its worker while its `files` grant, its agent
/// selector or its agent's configuration is withdrawn is refused there and
/// touches nothing. With nothing withdrawn, the same wait changes nothing.
#[tokio::test]
async fn a_workspace_operation_withdrawn_while_it_waits_does_not_take_effect() {
    type Withdraw = fn(&Arc<RpcContext>);
    let cases: [(&str, &str, Value, Withdraw); 3] = [
        (
            "the files grant",
            "fs/delete",
            json!({"agent": "alpha", "path": "notes/todo.md"}),
            |ctx| {
                let narrowed = files_alpha_narrowed(ctx, |profile| {
                    profile
                        .grants
                        .remove(&zeroclaw_api::grants::Resource::Files);
                });
                ctx.auth.refresh_from_config(&narrowed).unwrap();
            },
        ),
        (
            "the agent selector",
            "fs/mkdir",
            json!({"agent": "alpha", "path": "made"}),
            |ctx| {
                let narrowed = files_alpha_narrowed(ctx, |profile| {
                    profile.allowed_agents = vec!["beta".into()];
                });
                ctx.auth.refresh_from_config(&narrowed).unwrap();
            },
        ),
        (
            "the configured agent",
            "fs/move",
            json!({"agent": "alpha", "from": "notes", "to": "moved"}),
            |ctx| {
                ctx.config.write().agents.remove("alpha");
            },
        ),
    ];
    for (what, method, params, withdraw) in cases {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = enforcement_ctx(files_config(&tmp));
        let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;
        let refused = workspace_request_parked_on_its_worker(
            &mut peer,
            &mut rx,
            &ctx,
            1,
            method,
            params,
            || withdraw(&ctx),
        )
        .await;
        assert_forbidden(&refused, &format!("{method} after {what} was withdrawn"));
        let alpha = tmp.path().join("agents/alpha/workspace");
        assert_eq!(std::fs::read(alpha.join("notes/todo.md")).unwrap(), b"todo");
        assert!(!alpha.join("made").exists(), "{what}");
        assert!(!alpha.join("moved").exists(), "{what}");
    }

    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(files_config(&tmp));
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;
    let deleted = workspace_request_parked_on_its_worker(
        &mut peer,
        &mut rx,
        &ctx,
        1,
        "fs/delete",
        json!({"agent": "alpha", "path": "notes/todo.md"}),
        || {},
    )
    .await;
    assert_eq!(
        deleted["result"],
        json!({"removed": "notes/todo.md"}),
        "{deleted}"
    );
    assert!(
        !tmp.path()
            .join("agents/alpha/workspace/notes/todo.md")
            .exists()
    );
}

/// The worker holds the authority lease from its check through the effect,
/// so a policy change that arrives after the check waits for the operation:
/// the withdrawal lands after the file is gone, never between the check and
/// the delete, and the next request is refused under it.
#[tokio::test]
async fn a_policy_change_after_the_workers_check_lands_after_the_effect() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(files_config(&tmp));
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;
    let target = tmp.path().join("agents/alpha/workspace/notes/todo.md");
    let narrowed = files_alpha_narrowed(&ctx, |profile| {
        profile
            .grants
            .remove(&zeroclaw_api::grants::Resource::Files);
    });

    let publisher = Arc::new(std::sync::Mutex::new(None));
    {
        let (ctx, publisher, target) = (Arc::clone(&ctx), Arc::clone(&publisher), target.clone());
        let hook_ctx = Arc::clone(&ctx);
        hook_ctx.sessions.set_test_workspace_effect_hook(move || {
            let handle = {
                let ctx = Arc::clone(&ctx);
                std::thread::spawn(move || {
                    ctx.auth.refresh_from_config(&narrowed).unwrap();
                    // Whether the delete had already happened when the
                    // publication completed.
                    !target.exists()
                })
            };
            let started = std::time::Instant::now();
            while !ctx.auth.publication_queued_behind_a_lease() {
                assert!(!handle.is_finished(), "the publication did not wait");
                assert!(
                    started.elapsed() < std::time::Duration::from_secs(10),
                    "the publication never reached the lease"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            *publisher.lock().unwrap() = Some(handle);
        });
    }

    let deleted = rpc(
        &mut peer,
        &mut rx,
        1,
        "fs/delete",
        json!({"agent": "alpha", "path": "notes/todo.md"}),
    )
    .await;
    assert_eq!(
        deleted["result"],
        json!({"removed": "notes/todo.md"}),
        "{deleted}"
    );
    let handle = publisher
        .lock()
        .unwrap()
        .take()
        .expect("the effect hook ran and queued the publication");
    assert!(
        handle.join().unwrap(),
        "the publication completed only after the delete"
    );

    let refused = rpc(
        &mut peer,
        &mut rx,
        2,
        "fs/read",
        json!({"agent": "alpha", "path": "notes"}),
    )
    .await;
    assert_forbidden(&refused, "a read after the files grant was withdrawn");
}

/// A path past the browse bound is an invalid path on the RPC surface, and
/// nothing is made for it.
#[tokio::test]
async fn an_overdeep_path_is_refused_before_anything_is_made() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(files_config(&tmp));
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;
    let deep = vec!["d"; crate::browse::MAX_PATH_COMPONENTS + 1].join("/");

    let made = rpc(
        &mut peer,
        &mut rx,
        1,
        "fs/mkdir",
        json!({"agent": "alpha", "path": deep}),
    )
    .await;
    let moved = rpc(
        &mut peer,
        &mut rx,
        2,
        "fs/move",
        json!({"agent": "alpha", "from": "notes", "to": deep}),
    )
    .await;
    for response in [made, moved] {
        assert_eq!(
            response["error"]["code"],
            json!(zeroclaw_api::jsonrpc::error_codes::FS_INVALID_PATH),
            "{response}"
        );
    }
    let alpha = tmp.path().join("agents/alpha/workspace");
    assert!(!alpha.join("d").exists());
    assert!(alpha.join("notes/todo.md").is_file());
}

#[test]
fn workspace_methods_are_classified_and_named() {
    use zeroclaw_api::grants::{Resource, Verb};
    for (method, wire, verb) in [
        (Method::WorkspaceList, "workspace/list", Verb::Read),
        (Method::FsRead, "fs/read", Verb::Read),
        (Method::FsMkdir, "fs/mkdir", Verb::Create),
        (Method::FsMove, "fs/move", Verb::Update),
        (Method::FsRmdir, "fs/rmdir", Verb::Delete),
        (Method::FsDelete, "fs/delete", Verb::Delete),
    ] {
        assert_eq!(method.wire_name(), wire);
        assert_eq!(Method::from_wire(wire), Some(method));
        assert_eq!(
            method.authz(),
            MethodAuthz::Requires(Resource::Files, verb),
            "{wire}"
        );
    }
}

#[test]
fn catalog_methods_are_classified_and_named() {
    use zeroclaw_api::grants::{Resource, Verb};
    for (method, wire, resource) in [
        (
            Method::IntegrationsList,
            "integrations/list",
            Resource::Tools,
        ),
        (
            Method::ToolsCliDiscover,
            "tools/cli-discover",
            Resource::Tools,
        ),
        (Method::PluginsList, "plugins/list", Resource::Plugins),
        (Method::A2aIdentity, "a2a/identity", Resource::System),
    ] {
        assert_eq!(method.wire_name(), wire);
        assert_eq!(Method::from_wire(wire), Some(method));
        assert_eq!(
            method.authz(),
            MethodAuthz::Requires(resource, Verb::Read),
            "{wire}"
        );
    }
}

/// `scoped` holds `system:read` and `tools:read` for agent `alpha` only.
fn catalog_config(tmp: &tempfile::TempDir) -> zeroclaw_config::schema::Config {
    use std::collections::HashMap;
    use zeroclaw_api::grants::{Resource, Verb};
    use zeroclaw_config::schema::{AliasedAgentConfig, PermissionProfileConfig, UserConfig};

    let mut config = zeroclaw_config::schema::Config {
        config_path: tmp.path().join("config.toml"),
        ..zeroclaw_config::schema::Config::default()
    };
    config.a2a.server.enabled = true;
    for alias in ["alpha", "beta"] {
        let mut agent = AliasedAgentConfig {
            enabled: true,
            ..Default::default()
        };
        agent.a2a.published = true;
        config.agents.insert(alias.into(), agent);
    }
    config.permission_profiles.insert(
        "catalog-alpha".into(),
        PermissionProfileConfig {
            allowed_agents: vec!["alpha".into()],
            grants: HashMap::from([
                (Resource::System, vec![Verb::Read]),
                (Resource::Tools, vec![Verb::Read]),
            ]),
            ..PermissionProfileConfig::default()
        },
    );
    config.users.insert(
        "scoped".into(),
        UserConfig {
            uid: Some(SCOPED),
            permission_profiles: vec!["catalog-alpha".into()],
            ..UserConfig::default()
        },
    );
    config
}

#[tokio::test]
async fn a2a_identity_holds_a_named_agent_to_the_selector() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(catalog_config(&tmp));
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;

    let own = rpc(
        &mut peer,
        &mut rx,
        1,
        "a2a/identity",
        json!({"agent": "alpha"}),
    )
    .await;
    assert_eq!(own["result"]["name"], json!("alpha"), "{own}");
    let other = rpc(
        &mut peer,
        &mut rx,
        2,
        "a2a/identity",
        json!({"agent": "beta"}),
    )
    .await;
    assert_forbidden(&other, "another agent's card");
    // The catalog lists published agents only, like the unauthenticated
    // well-known route, so it is not held to the selector.
    let catalog = rpc(&mut peer, &mut rx, 3, "a2a/identity", json!({})).await;
    assert_eq!(
        catalog["result"]["name"],
        json!("ZeroClaw agents"),
        "{catalog}"
    );
}

#[tokio::test]
async fn catalog_methods_route_through_the_gate() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(catalog_config(&tmp));
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;

    let integrations = rpc(&mut peer, &mut rx, 1, "integrations/list", json!({})).await;
    assert!(
        integrations["result"]["integrations"].is_array(),
        "{integrations}"
    );
    // `plugins:read` is not granted, so the gate refuses before the handler.
    let plugins = rpc(&mut peer, &mut rx, 2, "plugins/list", json!({})).await;
    assert_forbidden(&plugins, "plugins/list without plugins:read");
}

// ── Canvas ────────────────────────────────────────────────────────────────

/// Before the daemon's canvas store reached the RPC context, an RPC-built
/// agent's canvas tool wrote to a private store no reader could see. It now
/// draws into the shared store, under its agent's namespace.
#[tokio::test]
async fn a_canvas_drawn_by_an_rpc_built_agent_is_the_one_canvas_rpc_serves() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(make_acp_test_config(&tmp));
    let (mut operator, mut rx) = local_operator(&ctx).await;
    let created = rpc(
        &mut operator,
        &mut rx,
        1,
        "session/new",
        json!({"agent_alias": "test-agent", "session_id": "s-canvas"}),
    )
    .await;
    assert_eq!(
        created["result"]["session_id"],
        json!("s-canvas"),
        "{created}"
    );

    let agent = ctx
        .sessions
        .get_agent("s-canvas")
        .await
        .expect("session exists");
    let drawn = agent
        .lock()
        .await
        .execute_tool_for_test(
            "canvas",
            json!({
                "action": "render",
                "canvas_id": "board",
                "content_type": "text",
                "content": "drawn by the agent",
            }),
        )
        .await
        .expect("the agent has the canvas tool")
        .expect("the canvas tool runs");
    assert!(drawn.success, "{drawn:?}");

    let got = rpc(
        &mut operator,
        &mut rx,
        2,
        "canvas/get",
        json!({"canvas_id": "test-agent/board"}),
    )
    .await;
    assert_eq!(
        got["result"]["frame"]["content"],
        json!("drawn by the agent"),
        "{got}"
    );
    let listed = rpc(&mut operator, &mut rx, 3, "canvas/list", json!({})).await;
    assert_eq!(
        listed["result"]["canvases"],
        json!(["test-agent/board"]),
        "an agent's canvas is listed under its agent's namespace: {listed}"
    );
}

#[tokio::test]
async fn canvas_render_refuses_what_the_route_refuses() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(make_acp_test_config(&tmp));
    let (mut operator, mut rx) = local_operator(&ctx).await;

    let eval = rpc(
        &mut operator,
        &mut rx,
        1,
        "canvas/render",
        json!({"canvas_id": "c", "content_type": "eval", "content": "alert(1)"}),
    )
    .await;
    assert_eq!(eval["error"]["code"], json!(INVALID_PARAMS), "{eval}");
    let huge = "x".repeat(crate::tools::MAX_CONTENT_SIZE + 1);
    let too_large = rpc(
        &mut operator,
        &mut rx,
        2,
        "canvas/render",
        json!({"canvas_id": "c", "content": huge}),
    )
    .await;
    assert_eq!(
        too_large["error"]["code"],
        json!(INVALID_PARAMS),
        "{too_large}"
    );
    let missing = rpc(
        &mut operator,
        &mut rx,
        3,
        "canvas/get",
        json!({"canvas_id": "nope"}),
    )
    .await;
    assert_eq!(
        missing["error"]["message"],
        json!("Canvas 'nope' not found"),
        "{missing}"
    );
    assert!(ctx.canvas_store.list().is_empty(), "nothing was rendered");

    let rendered = rpc(
        &mut operator,
        &mut rx,
        4,
        "canvas/render",
        json!({"canvas_id": "c", "content": "<p>ok</p>"}),
    )
    .await;
    assert_eq!(
        rendered["result"]["frame"]["content_type"],
        json!("html"),
        "{rendered}"
    );
    let cleared = rpc(
        &mut operator,
        &mut rx,
        5,
        "canvas/clear",
        json!({"canvas_id": "c"}),
    )
    .await;
    assert_eq!(
        cleared["result"],
        json!({"canvas_id": "c", "status": "cleared"}),
        "{cleared}"
    );
}

#[tokio::test]
async fn canvas_methods_need_a_canvas_grant() {
    // `files-alpha` grants files only, so every canvas method is refused.
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(files_config(&tmp));
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;
    for (id, method, params) in [
        (1, "canvas/list", json!({})),
        (2, "canvas/get", json!({"canvas_id": "c"})),
        (3, "canvas/history", json!({"canvas_id": "c"})),
        (
            4,
            "canvas/render",
            json!({"canvas_id": "c", "content": "x"}),
        ),
        (5, "canvas/clear", json!({"canvas_id": "c"})),
    ] {
        let response = rpc(&mut peer, &mut rx, id, method, params).await;
        assert_forbidden(&response, method);
    }
    assert!(ctx.canvas_store.list().is_empty());
}

#[test]
fn canvas_methods_are_classified_and_named() {
    use zeroclaw_api::grants::{Resource, Verb};
    for (method, wire, verb) in [
        (Method::CanvasList, "canvas/list", Verb::Read),
        (Method::CanvasGet, "canvas/get", Verb::Read),
        (Method::CanvasHistory, "canvas/history", Verb::Read),
        (Method::CanvasRender, "canvas/render", Verb::Update),
        (Method::CanvasClear, "canvas/clear", Verb::Delete),
    ] {
        assert_eq!(method.wire_name(), wire);
        assert_eq!(Method::from_wire(wire), Some(method));
        assert_eq!(
            method.authz(),
            MethodAuthz::Requires(Resource::Canvas, verb),
            "{wire}"
        );
    }
}

// ── Tool listing ──────────────────────────────────────────────────────────

#[test]
fn tools_list_is_classified_and_named() {
    use zeroclaw_api::grants::{Resource, Verb};
    assert_eq!(Method::ToolsList.wire_name(), "tools/list");
    assert_eq!(Method::from_wire("tools/list"), Some(Method::ToolsList));
    assert_eq!(
        Method::ToolsList.authz(),
        MethodAuthz::Requires(Resource::Tools, Verb::Read)
    );
}

#[tokio::test]
async fn tools_list_assembles_the_agents_tools_on_request() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(make_acp_test_config(&tmp));
    let (mut operator, mut rx) = local_operator(&ctx).await;

    for (id, params) in [(1, json!({"agent": "test-agent"})), (2, json!({}))] {
        let listed = rpc(&mut operator, &mut rx, id, "tools/list", params).await;
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("tools array: {listed}"))
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        for expected in ["canvas", "calculator"] {
            assert!(names.contains(&expected), "{expected} in {names:?}");
        }
    }
}

#[tokio::test]
async fn tools_list_refuses_an_agent_that_does_not_resolve() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut config = make_acp_test_config(&tmp);
    let mut disabled = config.agents["test-agent"].clone();
    disabled.enabled = false;
    config.agents.insert("dormant".into(), disabled);
    let ctx = enforcement_ctx(config);
    let (mut operator, mut rx) = local_operator(&ctx).await;
    for (id, agent) in [(1, "nobody"), (2, "dormant")] {
        let response = rpc(
            &mut operator,
            &mut rx,
            id,
            "tools/list",
            json!({"agent": agent}),
        )
        .await;
        assert_eq!(
            response["error"]["code"],
            json!(INVALID_PARAMS),
            "{agent}: {response}"
        );
    }
}

#[tokio::test]
async fn tools_list_holds_the_agent_to_the_selector() {
    // `catalog-alpha` holds tools:read for agent alpha only.
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(catalog_config(&tmp));
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;
    let other = rpc(
        &mut peer,
        &mut rx,
        1,
        "tools/list",
        json!({"agent": "beta"}),
    )
    .await;
    assert_forbidden(&other, "another agent's tools");
}

// ── Metrics ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn metrics_scrape_reports_the_hint_without_the_prometheus_backend() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(make_acp_test_config(&tmp));
    let (mut operator, mut rx) = local_operator(&ctx).await;
    let scraped = rpc(&mut operator, &mut rx, 1, "metrics/scrape", json!({})).await;
    assert_eq!(
        scraped["result"]["text"],
        json!(crate::observability::PROMETHEUS_DISABLED_HINT),
        "{scraped}"
    );
    assert_eq!(
        scraped["result"]["content_type"],
        json!(crate::observability::PROMETHEUS_CONTENT_TYPE)
    );
}

#[test]
fn metrics_scrape_is_classified_and_named() {
    use zeroclaw_api::grants::{Resource, Verb};
    assert_eq!(Method::MetricsScrape.wire_name(), "metrics/scrape");
    assert_eq!(
        Method::from_wire("metrics/scrape"),
        Some(Method::MetricsScrape)
    );
    assert_eq!(
        Method::MetricsScrape.authz(),
        MethodAuthz::Requires(Resource::System, Verb::Read)
    );
}

// ── Pairing ───────────────────────────────────────────────────────────────

/// A config with pairing required and its data directory (where the device
/// registry lives) inside `tmp`, never the real home directory.
fn pairing_config(
    tmp: &tempfile::TempDir,
    require_pairing: bool,
) -> zeroclaw_config::schema::Config {
    let mut config = make_acp_test_config(tmp);
    config.data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&config.data_dir).unwrap();
    config.config_path = tmp.path().join("config.toml");
    config.gateway.require_pairing = require_pairing;
    config
}

#[test]
fn pairing_methods_are_classified_and_named() {
    use zeroclaw_api::grants::{Resource, Verb};
    for (method, wire, verb) in [
        (Method::PairingList, "pairing/list", Verb::Read),
        (Method::PairingRevoke, "pairing/revoke", Verb::Delete),
        (Method::PairingRevokeAll, "pairing/revoke-all", Verb::Delete),
        (Method::PairingNewCode, "pairing/new-code", Verb::Create),
    ] {
        assert_eq!(method.wire_name(), wire);
        assert_eq!(Method::from_wire(wire), Some(method));
        assert_eq!(
            method.authz(),
            MethodAuthz::Requires(Resource::System, verb),
            "{wire}"
        );
    }
}

#[test]
fn an_administrator_manages_pairing_over_rpc() {
    // Persisting the pairing tokens runs the config save, whose debug-build
    // frames exceed the default test stack; see `run_on_a_large_stack`.
    run_on_a_large_stack(|| async move {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = enforcement_ctx(pairing_config(&tmp, true));
        let (mut operator, mut rx) = local_operator(&ctx).await;

        let code = rpc(&mut operator, &mut rx, 1, "pairing/new-code", json!({})).await;
        assert_eq!(code["result"]["success"], json!(true), "{code}");
        assert!(code["result"]["pairing_code"].is_string(), "{code}");

        let listed = rpc(&mut operator, &mut rx, 2, "pairing/list", json!({})).await;
        assert_eq!(listed["result"]["count"], json!(0), "{listed}");

        let missing = rpc(
            &mut operator,
            &mut rx,
            3,
            "pairing/revoke",
            json!({"device_id": "no-such-device"}),
        )
        .await;
        assert_eq!(missing["error"]["code"], json!(INVALID_PARAMS), "{missing}");
        assert_eq!(missing["error"]["message"], json!("Device not found"));

        let rotated = rpc(&mut operator, &mut rx, 4, "pairing/revoke-all", json!({})).await;
        assert_eq!(rotated["result"]["success"], json!(true), "{rotated}");
        assert!(
            rotated["result"]["message"]
                .as_str()
                .is_some_and(|m| m.starts_with("Revoked all")),
            "{rotated}"
        );
    });
}

#[tokio::test]
async fn pairing_methods_refuse_a_principal_that_is_not_an_administrator() {
    use std::collections::HashMap;
    use zeroclaw_api::grants::{Resource, Verb};
    use zeroclaw_config::schema::{PermissionProfileConfig, UserConfig};

    let tmp = tempfile::TempDir::new().unwrap();
    let mut config = pairing_config(&tmp, true);
    config.permission_profiles.insert(
        "system-everything".into(),
        PermissionProfileConfig {
            allowed_agents: vec![zeroclaw_api::grants::WILDCARD.into()],
            grants: HashMap::from([(
                Resource::System,
                vec![
                    Verb::Create,
                    Verb::Read,
                    Verb::Update,
                    Verb::Delete,
                    Verb::Execute,
                ],
            )]),
            ..PermissionProfileConfig::default()
        },
    );
    config.users.insert(
        "scoped".into(),
        UserConfig {
            uid: Some(SCOPED),
            permission_profiles: vec!["system-everything".into()],
            ..UserConfig::default()
        },
    );
    let ctx = enforcement_ctx(config);
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;
    for (id, method, params) in [
        (1, "pairing/list", json!({})),
        (2, "pairing/revoke", json!({"device_id": "d"})),
        (3, "pairing/revoke-all", json!({})),
        (4, "pairing/new-code", json!({})),
    ] {
        let response = rpc(&mut peer, &mut rx, id, method, params).await;
        assert_forbidden(&response, method);
        assert!(
            response["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("administrator")),
            "{method} is refused by the administrator check, not the grant gate: {response}"
        );
    }
}

#[tokio::test]
async fn pairing_new_code_reports_pairing_disabled() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(pairing_config(&tmp, false));
    let (mut operator, mut rx) = local_operator(&ctx).await;
    let code = rpc(&mut operator, &mut rx, 1, "pairing/new-code", json!({})).await;
    assert_eq!(code["error"]["code"], json!(INVALID_REQUEST), "{code}");
    assert_eq!(
        code["error"]["message"],
        json!("Pairing is disabled for this gateway")
    );
}

// ── Channels ──────────────────────────────────────────────────────────────

/// Records the calls it receives and answers each with a fixed body.
#[derive(Default)]
struct RecordingChannels {
    calls: parking_lot::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl crate::rpc::channels::ChannelControl for RecordingChannels {
    fn list(
        &self,
        _config: &zeroclaw_config::schema::Config,
        _pairing: &zeroclaw_config::pairing::PairingGuard,
    ) -> Value {
        self.calls.lock().push("list".into());
        json!({"channels": []})
    }

    fn relink(
        &self,
        _config: &zeroclaw_config::schema::Config,
        channel: &str,
    ) -> Result<Value, JsonRpcError> {
        self.calls.lock().push(format!("relink {channel}"));
        Ok(json!({"channel": channel, "outcome": "nothing_to_clear"}))
    }

    async fn bind(
        &self,
        config: &Arc<parking_lot::RwLock<zeroclaw_config::schema::Config>>,
        _config_write_guard: &tokio::sync::OwnedMutexGuard<()>,
        channel_type: &str,
        alias: &str,
        identity: &str,
        authorize_write: &(
             dyn for<'p> Fn(&'p str, zeroclaw_api::grants::Verb) -> Result<(), JsonRpcError>
                 + Send
                 + Sync
         ),
    ) -> Result<Value, JsonRpcError> {
        // The path and verb the real capability authorizes for a channel's
        // own group: a creation unless the group already exists.
        let group = format!("{channel_type}_{alias}");
        let verb = if config.read().peer_groups.contains_key(&group) {
            zeroclaw_api::grants::Verb::Update
        } else {
            zeroclaw_api::grants::Verb::Create
        };
        authorize_write(&format!("peer_groups.{group}.external_peers"), verb)?;
        self.calls
            .lock()
            .push(format!("bind {channel_type}.{alias} {identity}"));
        Ok(json!({"saved": true, "already_bound": false}))
    }
}

fn with_channels(
    config: zeroclaw_config::schema::Config,
) -> (Arc<RpcContext>, Arc<RecordingChannels>) {
    let mut ctx = enforcement_ctx(config);
    let channels = Arc::new(RecordingChannels::default());
    Arc::get_mut(&mut ctx)
        .expect("a fresh context is unshared")
        .channel_control =
        Some(Arc::clone(&channels) as Arc<dyn crate::rpc::channels::ChannelControl>);
    (ctx, channels)
}

#[test]
fn channels_methods_are_classified_and_named() {
    use zeroclaw_api::grants::{Resource, Verb};
    for (method, wire, verb) in [
        (Method::ChannelsList, "channels/list", Verb::Read),
        (Method::ChannelsRelink, "channels/relink", Verb::Update),
        (Method::ChannelsBind, "channels/bind", Verb::Update),
    ] {
        assert_eq!(method.wire_name(), wire);
        assert_eq!(Method::from_wire(wire), Some(method));
        assert_eq!(
            method.authz(),
            MethodAuthz::Requires(Resource::Channels, verb),
            "{wire}"
        );
    }
}

#[tokio::test]
async fn channels_methods_route_to_the_registered_capability() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (ctx, channels) = with_channels(make_acp_test_config(&tmp));
    let (mut operator, mut rx) = local_operator(&ctx).await;

    let listed = rpc(&mut operator, &mut rx, 1, "channels/list", json!({})).await;
    assert_eq!(listed["result"], json!({"channels": []}), "{listed}");
    let relinked = rpc(
        &mut operator,
        &mut rx,
        2,
        "channels/relink",
        json!({"channel": "whatsapp.main"}),
    )
    .await;
    assert_eq!(
        relinked["result"]["outcome"],
        json!("nothing_to_clear"),
        "{relinked}"
    );
    let bound = rpc(
        &mut operator,
        &mut rx,
        3,
        "channels/bind",
        json!({"channel_type": "telegram", "alias": "main", "identity": "@alice"}),
    )
    .await;
    assert_eq!(bound["result"]["saved"], json!(true), "{bound}");
    assert_eq!(
        *channels.calls.lock(),
        vec![
            "list".to_string(),
            "relink whatsapp.main".to_string(),
            "bind telegram.main @alice".to_string(),
        ]
    );
}

#[tokio::test]
async fn channels_methods_say_so_when_no_channels_run() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(make_acp_test_config(&tmp));
    let (mut operator, mut rx) = local_operator(&ctx).await;
    let listed = rpc(&mut operator, &mut rx, 1, "channels/list", json!({})).await;
    assert_eq!(listed["error"]["code"], json!(INVALID_REQUEST), "{listed}");
}

#[tokio::test]
async fn channels_bind_needs_the_peer_groups_config_write_grant() {
    use std::collections::HashMap;
    use zeroclaw_api::grants::{Resource, Verb};
    use zeroclaw_config::schema::{PermissionProfileConfig, UserConfig};

    let tmp = tempfile::TempDir::new().unwrap();
    let mut config = make_acp_test_config(&tmp);
    config.permission_profiles.insert(
        "channels-only".into(),
        PermissionProfileConfig {
            allowed_agents: vec![zeroclaw_api::grants::WILDCARD.into()],
            grants: HashMap::from([(Resource::Channels, vec![Verb::Read, Verb::Update])]),
            ..PermissionProfileConfig::default()
        },
    );
    config.users.insert(
        "scoped".into(),
        UserConfig {
            uid: Some(SCOPED),
            permission_profiles: vec!["channels-only".into()],
            ..UserConfig::default()
        },
    );
    let (ctx, channels) = with_channels(config);
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;

    let listed = rpc(&mut peer, &mut rx, 1, "channels/list", json!({})).await;
    assert!(
        listed["result"].is_object(),
        "channels:read suffices to list: {listed}"
    );
    let bound = rpc(
        &mut peer,
        &mut rx,
        2,
        "channels/bind",
        json!({"channel_type": "telegram", "alias": "main", "identity": "@mallory"}),
    )
    .await;
    assert_forbidden(&bound, "bind without a peer_groups config write grant");
    assert_eq!(
        *channels.calls.lock(),
        vec!["list".to_string()],
        "the bind never reached the capability"
    );
}

/// A bind is a config write, so it needs what the dashboard route asks of
/// one: the `config` verb its effect needs, not only the path. The first
/// bind for a channel creates its peer group; a later one updates it.
#[tokio::test]
async fn channels_bind_needs_the_config_verb_its_effect_needs() {
    use zeroclaw_api::grants::{Resource, Verb};

    let bind = json!({"channel_type": "telegram", "alias": "main", "identity": "@alice"});
    let with_config_verbs = |tmp: &tempfile::TempDir, verbs: Vec<Verb>, group_exists: bool| {
        let mut config = binder_config(tmp, &["*"], &["peer_groups.*"]);
        let grants = &mut config
            .permission_profiles
            .get_mut("binder")
            .expect("binder_config defines the binder profile")
            .grants;
        grants.remove(&Resource::Config);
        if !verbs.is_empty() {
            grants.insert(Resource::Config, verbs);
        }
        if group_exists {
            config
                .peer_groups
                .insert("telegram_main".into(), Default::default());
        }
        config
    };

    for (what, verbs, group_exists) in [
        ("a path grant alone", vec![], false),
        ("config:update for a new group", vec![Verb::Update], false),
        (
            "config:create for an existing group",
            vec![Verb::Create],
            true,
        ),
    ] {
        let tmp = tempfile::TempDir::new().unwrap();
        let (ctx, channels) = with_channels(with_config_verbs(&tmp, verbs, group_exists));
        let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;
        let bound = rpc(&mut peer, &mut rx, 1, "channels/bind", bind.clone()).await;
        assert_forbidden(&bound, what);
        assert!(channels.calls.lock().is_empty(), "{what}: nothing bound");
    }

    for (what, verbs, group_exists) in [
        ("config:create for a new group", vec![Verb::Create], false),
        (
            "config:update for an existing group",
            vec![Verb::Update],
            true,
        ),
    ] {
        let tmp = tempfile::TempDir::new().unwrap();
        let (ctx, channels) = with_channels(with_config_verbs(&tmp, verbs, group_exists));
        let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;
        let bound = rpc(&mut peer, &mut rx, 1, "channels/bind", bind.clone()).await;
        assert_eq!(bound["result"]["saved"], json!(true), "{what}: {bound}");
        assert_eq!(channels.calls.lock().len(), 1, "{what}");
    }
}

// ── System ────────────────────────────────────────────────────────────────
//
// None of these may start a real upgrade: that would run `zeroclaw update`
// on the test binary. Only refusals and status are exercised.

#[test]
fn system_methods_are_classified_and_named() {
    use zeroclaw_api::grants::{Resource, Verb};
    for (method, wire, verb) in [
        (Method::SystemUpgrade, "system/upgrade", Verb::Execute),
        (Method::SystemRestart, "system/restart", Verb::Execute),
        (
            Method::SystemUpgradeStatus,
            "system/upgrade-status",
            Verb::Read,
        ),
    ] {
        assert_eq!(method.wire_name(), wire);
        assert_eq!(Method::from_wire(wire), Some(method));
        assert_eq!(
            method.authz(),
            MethodAuthz::Requires(Resource::System, verb),
            "{wire}"
        );
    }
}

#[tokio::test]
async fn system_upgrade_honors_allow_self_upgrade() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut config = make_acp_test_config(&tmp);
    config.gateway.allow_self_upgrade = false;
    let ctx = enforcement_ctx(config);
    let (mut operator, mut rx) = local_operator(&ctx).await;
    let upgraded = rpc(&mut operator, &mut rx, 1, "system/upgrade", json!({})).await;
    assert_eq!(
        upgraded["error"]["code"],
        json!(INVALID_REQUEST),
        "{upgraded}"
    );
    assert!(
        upgraded["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("allow_self_upgrade")),
        "{upgraded}"
    );
}

#[tokio::test]
async fn system_restart_restarts_only_the_daemon_and_needs_a_supervisor() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(make_acp_test_config(&tmp));
    let (mut operator, mut rx) = local_operator(&ctx).await;
    let other = rpc(
        &mut operator,
        &mut rx,
        1,
        "system/restart",
        json!({"component": "gateway"}),
    )
    .await;
    assert_eq!(other["error"]["code"], json!(INVALID_PARAMS), "{other}");
    // The minimal context has no reload channel, as a daemon-less process.
    let daemon = rpc(
        &mut operator,
        &mut rx,
        2,
        "system/restart",
        json!({"component": "daemon"}),
    )
    .await;
    assert_eq!(daemon["error"]["code"], json!(INVALID_REQUEST), "{daemon}");
}

#[tokio::test]
async fn system_upgrade_and_restart_are_for_administrators() {
    use std::collections::HashMap;
    use zeroclaw_api::grants::{Resource, Verb};
    use zeroclaw_config::schema::{PermissionProfileConfig, UserConfig};

    let tmp = tempfile::TempDir::new().unwrap();
    let mut config = make_acp_test_config(&tmp);
    config.gateway.allow_self_upgrade = true;
    config.permission_profiles.insert(
        "system-exec".into(),
        PermissionProfileConfig {
            allowed_agents: vec![zeroclaw_api::grants::WILDCARD.into()],
            grants: HashMap::from([(Resource::System, vec![Verb::Read, Verb::Execute])]),
            ..PermissionProfileConfig::default()
        },
    );
    config.users.insert(
        "scoped".into(),
        UserConfig {
            uid: Some(SCOPED),
            permission_profiles: vec!["system-exec".into()],
            ..UserConfig::default()
        },
    );
    let ctx = enforcement_ctx(config);
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;
    for (id, method, params) in [
        (1, "system/upgrade", json!({})),
        (2, "system/restart", json!({"component": "daemon"})),
    ] {
        let response = rpc(&mut peer, &mut rx, id, method, params).await;
        assert_forbidden(&response, method);
    }
    // Reading progress needs only system:read.
    let status = rpc(&mut peer, &mut rx, 3, "system/upgrade-status", json!({})).await;
    assert!(status["result"]["state"].is_string(), "{status}");
}

/// A principal with `files:delete` for its agent cannot reach that agent's
/// workspace root, or the shared root, by folding `..` into the path.
#[tokio::test]
async fn fs_delete_and_rmdir_refuse_a_path_that_resolves_to_the_root() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(files_config(&tmp));
    let (mut scoped, mut rx) = roster_peer(&ctx, SCOPED).await;
    for (id, path) in [(1, "notes/.."), (2, "x/..")] {
        let response = rpc(
            &mut scoped,
            &mut rx,
            id,
            "fs/delete",
            json!({"agent": "alpha", "path": path}),
        )
        .await;
        assert_eq!(
            response["error"]["code"],
            json!(zeroclaw_api::jsonrpc::error_codes::FS_PERMISSION_DENIED),
            "{path}: {response}"
        );
    }
    assert!(
        tmp.path()
            .join("agents/alpha/workspace/notes/todo.md")
            .exists()
    );

    let (mut wildcard, mut wildcard_rx) = roster_peer(&ctx, WILDCARD_UID).await;
    let response = rpc(
        &mut wildcard,
        &mut wildcard_rx,
        3,
        "fs/rmdir",
        json!({"path": "made/.."}),
    )
    .await;
    assert_eq!(
        response["error"]["code"],
        json!(zeroclaw_api::jsonrpc::error_codes::FS_PERMISSION_DENIED),
        "{response}"
    );
    assert!(tmp.path().join("shared").exists());
}

// ── Review hardening ──────────────────────────────────────────────────────

#[tokio::test]
async fn minting_and_clearing_pairing_credentials_is_local_only() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(pairing_config(&tmp, true));
    let (writer, _rx) = tokio::sync::mpsc::channel(8);
    let remote = RpcDispatcher::new(Arc::clone(&ctx), writer, "wss:test".into()).with_transport(
        crate::rpc::transport::TransportKind::Wss,
        crate::security::auth_provider::Credential::None,
    );
    for method in [Method::PairingNewCode, Method::PairingRevokeAll] {
        let refused = remote.require_local_transport(method).unwrap_err();
        assert_eq!(refused.code, FORBIDDEN, "{}", method.wire_name());
    }
    let (writer, _rx) = tokio::sync::mpsc::channel(8);
    let local = RpcDispatcher::new(Arc::clone(&ctx), writer, "local:test".into());
    assert!(
        local
            .require_local_transport(Method::PairingNewCode)
            .is_ok()
    );
}

const PAIRED_TOKEN: &str = "p6-paired-token";

fn paired_ctx(tmp: &tempfile::TempDir) -> Arc<RpcContext> {
    let mut config = pairing_config(tmp, true);
    config.gateway.paired_tokens = vec![PAIRED_TOKEN.to_string()];
    let ctx = enforcement_ctx(config);
    assert!(
        ctx.auth.pairing().is_authenticated(PAIRED_TOKEN),
        "the fixture token is paired"
    );
    ctx
}

#[test]
fn pairing_revoke_invalidates_a_registered_devices_token() {
    // Persisting the pairing tokens runs the config save, whose debug-build
    // frames exceed the default test stack; see `run_on_a_large_stack`.
    run_on_a_large_stack(|| async move {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = paired_ctx(&tmp);
        let now = chrono::Utc::now();
        crate::devices::DeviceRegistry::shared(&ctx.config.read().data_dir)
            .register(
                zeroclaw_config::pairing::PairingGuard::token_hash(PAIRED_TOKEN),
                crate::devices::DeviceInfo {
                    id: "device-1".into(),
                    name: Some("phone".into()),
                    device_type: None,
                    paired_at: now,
                    last_seen: now,
                    ip_address: None,
                    capabilities: None,
                },
            )
            .unwrap();
        let (mut operator, mut rx) = local_operator(&ctx).await;

        let listed = rpc(&mut operator, &mut rx, 1, "pairing/list", json!({})).await;
        assert_eq!(listed["result"]["count"], json!(1), "{listed}");
        let revoked = rpc(
            &mut operator,
            &mut rx,
            2,
            "pairing/revoke",
            json!({"device_id": "device-1"}),
        )
        .await;
        assert_eq!(
            revoked["result"]["device_id"],
            json!("device-1"),
            "{revoked}"
        );
        assert!(
            !ctx.auth.pairing().is_authenticated(PAIRED_TOKEN),
            "the revoked device's bearer no longer authenticates"
        );
    });
}

#[test]
fn pairing_revoke_all_invalidates_every_paired_token() {
    // Persisting the pairing tokens runs the config save, whose debug-build
    // frames exceed the default test stack; see `run_on_a_large_stack`.
    run_on_a_large_stack(|| async move {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = paired_ctx(&tmp);
        let (mut operator, mut rx) = local_operator(&ctx).await;
        let rotated = rpc(&mut operator, &mut rx, 1, "pairing/revoke-all", json!({})).await;
        assert_eq!(rotated["result"]["success"], json!(true), "{rotated}");
        assert!(!ctx.auth.pairing().is_authenticated(PAIRED_TOKEN));
    });
}

#[tokio::test]
async fn system_restart_signals_the_daemon_supervisor() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut ctx = enforcement_ctx(make_acp_test_config(&tmp));
    let (reload_tx, mut reload_rx) = tokio::sync::watch::channel(false);
    Arc::get_mut(&mut ctx)
        .expect("a fresh context is unshared")
        .reload_tx = Some(reload_tx);
    let (mut operator, mut rx) = local_operator(&ctx).await;
    let restarted = rpc(
        &mut operator,
        &mut rx,
        1,
        "system/restart",
        json!({"component": "daemon"}),
    )
    .await;
    assert_eq!(
        restarted["result"],
        json!({"component": "daemon", "restarting": true}),
        "{restarted}"
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), reload_rx.changed())
        .await
        .expect("the supervisor is signalled")
        .expect("the reload channel stays open");
    assert!(*reload_rx.borrow());
}

/// A principal scoped to one agent, with `channels:*` and `canvas:read`.
fn one_agent_config(tmp: &tempfile::TempDir) -> zeroclaw_config::schema::Config {
    use std::collections::HashMap;
    use zeroclaw_api::grants::{Resource, Verb};
    use zeroclaw_config::schema::{PermissionProfileConfig, UserConfig};

    let mut config = make_acp_test_config(tmp);
    config.permission_profiles.insert(
        "one-agent".into(),
        PermissionProfileConfig {
            allowed_agents: vec!["test-agent".into()],
            grants: HashMap::from([
                (Resource::Channels, vec![Verb::Read, Verb::Update]),
                (Resource::Canvas, vec![Verb::Read]),
            ]),
            ..PermissionProfileConfig::default()
        },
    );
    config.users.insert(
        "scoped".into(),
        UserConfig {
            uid: Some(SCOPED),
            permission_profiles: vec!["one-agent".into()],
            ..UserConfig::default()
        },
    );
    config
}

#[tokio::test]
async fn channels_relink_of_a_channel_the_principal_does_not_own_is_refused() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (ctx, channels) = with_channels(one_agent_config(&tmp));
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;
    let relinked = rpc(
        &mut peer,
        &mut rx,
        1,
        "channels/relink",
        json!({"channel": "telegram.unowned"}),
    )
    .await;
    assert_forbidden(&relinked, "relink of a channel no agent of theirs owns");
    assert!(
        channels.calls.lock().is_empty(),
        "the capability was never reached"
    );
}

#[tokio::test]
async fn canvas_needs_access_to_every_agent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(one_agent_config(&tmp));
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;
    let listed = rpc(&mut peer, &mut rx, 1, "canvas/list", json!({})).await;
    assert_forbidden(&listed, "canvas/list scoped to one agent");
    assert!(
        listed["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("every agent")),
        "refused by the shared-store check, not the grant gate: {listed}"
    );
}

/// A principal entitled to one agent, allowed to open sessions on it and to
/// run its `canvas` tool, and nothing else.
fn scoped_canvas_session_config(tmp: &tempfile::TempDir) -> zeroclaw_config::schema::Config {
    use std::collections::HashMap;
    use zeroclaw_api::grants::{Resource, Verb};
    use zeroclaw_config::schema::{PermissionProfileConfig, UserConfig};

    let mut config = make_acp_test_config(tmp);
    config.permission_profiles.insert(
        "one-agent-canvas".into(),
        PermissionProfileConfig {
            allowed_agents: vec!["test-agent".into()],
            allowed_tools: vec!["canvas".into()],
            grants: HashMap::from([
                (
                    Resource::Sessions,
                    vec![Verb::Create, Verb::Read, Verb::Execute],
                ),
                (Resource::Tools, vec![Verb::Execute]),
            ]),
            ..PermissionProfileConfig::default()
        },
    );
    config.users.insert(
        "scoped".into(),
        UserConfig {
            uid: Some(SCOPED),
            permission_profiles: vec!["one-agent-canvas".into()],
            ..UserConfig::default()
        },
    );
    config
}

/// The shared canvas store is refused to a scoped principal on `canvas/*`,
/// and its session's canvas tool must not reach it either: the tool gets a
/// store of its own, so it can neither read, overwrite nor clear a frame
/// another agent drew.
#[tokio::test]
async fn a_scoped_sessions_canvas_tool_cannot_reach_the_shared_store() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(scoped_canvas_session_config(&tmp));

    // Another agent's frame, drawn into the shared store.
    let (mut operator, mut op_rx) = local_operator(&ctx).await;
    let seeded = rpc(
        &mut operator,
        &mut op_rx,
        1,
        "canvas/render",
        json!({"canvas_id": "default", "content_type": "text", "content": "beta's frame"}),
    )
    .await;
    assert!(seeded["error"].is_null(), "{seeded}");

    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;
    let created = rpc(
        &mut peer,
        &mut rx,
        1,
        "session/new",
        json!({"agent_alias": "test-agent", "session_id": "s-scoped-canvas"}),
    )
    .await;
    assert_eq!(
        created["result"]["session_id"],
        json!("s-scoped-canvas"),
        "{created}"
    );
    let agent = ctx
        .sessions
        .get_agent("s-scoped-canvas")
        .await
        .expect("session exists");
    let agent = agent.lock().await;
    let run = |args: Value| {
        let agent = &agent;
        async move {
            agent
                .execute_tool_for_test("canvas", args)
                .await
                .expect("the scoped session has the canvas tool")
                .expect("the canvas tool runs")
        }
    };

    let snapshot = run(json!({"action": "snapshot", "canvas_id": "default"})).await;
    assert!(
        !format!("{snapshot:?}").contains("beta's frame"),
        "the scoped session read another agent's frame: {snapshot:?}"
    );
    let rendered = run(json!({
        "action": "render",
        "canvas_id": "default",
        "content_type": "text",
        "content": "written by the scoped session",
    }))
    .await;
    assert!(rendered.success, "{rendered:?}");
    let _ = run(json!({"action": "clear", "canvas_id": "default"})).await;

    let got = rpc(
        &mut operator,
        &mut op_rx,
        2,
        "canvas/get",
        json!({"canvas_id": "default"}),
    )
    .await;
    assert_eq!(
        got["result"]["frame"]["content"],
        json!("beta's frame"),
        "the shared frame was neither overwritten nor cleared: {got}"
    );
}

// ── Adversarial review ────────────────────────────────────────────────────

/// A link in agent alpha's workspace into agent beta's does not let a
/// principal scoped to alpha read or delete beta's files.
#[cfg(unix)]
#[tokio::test]
async fn a_scoped_principal_cannot_follow_a_link_into_another_agents_workspace() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(files_config(&tmp));
    let alpha = tmp.path().join("agents/alpha/workspace");
    let beta = tmp.path().join("agents/beta/workspace");
    std::os::unix::fs::symlink(&beta, alpha.join("export")).unwrap();
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;
    for (id, method) in [(1, "fs/read"), (2, "fs/delete")] {
        let response = rpc(
            &mut peer,
            &mut rx,
            id,
            method,
            json!({"agent": "alpha", "path": "export/secret.md"}),
        )
        .await;
        assert_eq!(
            response["error"]["code"],
            json!(zeroclaw_api::jsonrpc::error_codes::FS_INVALID_PATH),
            "{method}: {response}"
        );
    }
    assert!(beta.join("secret.md").exists(), "beta's file survives");
}

/// A principal that may bind on `telegram.main`, owned by agent
/// `test-agent`: `scoped` holds `channels:update`, the agents in `agents`,
/// and config write access to `paths`. Agent `beta` is configured too, so the
/// channel can be handed to an agent the principal may not use.
fn binder_config(
    tmp: &tempfile::TempDir,
    agents: &[&str],
    paths: &[&str],
) -> zeroclaw_config::schema::Config {
    use std::collections::HashMap;
    use zeroclaw_api::grants::{Resource, Verb};
    use zeroclaw_config::schema::{
        AliasedAgentConfig, PermissionProfileConfig, TelegramConfig, UserConfig,
    };

    let mut config = make_acp_test_config(tmp);
    config.channels.telegram.insert(
        "main".into(),
        TelegramConfig {
            enabled: true,
            ..TelegramConfig::default()
        },
    );
    config
        .agents
        .get_mut("test-agent")
        .expect("the base fixture configures test-agent")
        .channels = vec!["telegram.main".into()];
    config.agents.insert(
        "beta".into(),
        AliasedAgentConfig {
            enabled: true,
            ..Default::default()
        },
    );
    config.permission_profiles.insert(
        "binder".into(),
        PermissionProfileConfig {
            allowed_agents: agents.iter().map(|a| (*a).to_string()).collect(),
            config_write_paths: paths.iter().map(|p| (*p).to_string()).collect(),
            grants: HashMap::from([
                (Resource::Channels, vec![Verb::Read, Verb::Update]),
                (Resource::Config, vec![Verb::Create, Verb::Update]),
            ]),
            ..PermissionProfileConfig::default()
        },
    );
    config.users.insert(
        "scoped".into(),
        UserConfig {
            uid: Some(SCOPED),
            permission_profiles: vec!["binder".into()],
            ..UserConfig::default()
        },
    );
    config
}

/// Send a `channels/bind` for `telegram.main` while another writer holds the
/// config write lock, apply `change` to the config and publish it as the
/// accepted policy while the bind waits, then release the lock and return
/// the bind's response.
async fn bind_while_parked(
    ctx: &Arc<RpcContext>,
    peer: &mut RpcDispatcher,
    rx: &mut tokio::sync::mpsc::Receiver<String>,
    change: impl FnOnce(&mut zeroclaw_config::schema::Config),
) -> Value {
    let held = Arc::clone(&ctx.config_write_lock).lock_owned().await;
    let bind = rpc(
        peer,
        rx,
        1,
        "channels/bind",
        json!({"channel_type": "telegram", "alias": "main", "identity": "123456789"}),
    );
    let change_the_world = async {
        // Let the bind pass its entry checks and park on the lock.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let mut changed = ctx.config.read().clone();
        change(&mut changed);
        *ctx.config.write() = changed.clone();
        let revision = ctx.auth.accepted_revision().saturating_add(1);
        ctx.auth
            .publish_accepted(&changed, revision)
            .expect("the changed policy publishes");
        drop(held);
    };
    let (response, ()) = tokio::join!(bind, change_the_world);
    response
}

/// A bind queued behind another config writer re-resolves its authority
/// once it holds the lock: a grant withdrawn while it waited stops it before
/// anything is written.
#[tokio::test]
async fn channels_bind_narrowed_after_admission_has_no_effect() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (ctx, channels) = with_channels(binder_config(&tmp, &["*"], &["peer_groups.*"]));
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;
    let response = bind_while_parked(&ctx, &mut peer, &mut rx, |config| {
        config
            .permission_profiles
            .get_mut("binder")
            .expect("the fixture profile exists")
            .config_write_paths
            .clear();
    })
    .await;
    assert_forbidden(
        &response,
        "a bind whose grant was withdrawn while it waited",
    );
    assert!(
        channels.calls.lock().is_empty(),
        "nothing reached the bind after authority was withdrawn"
    );
}

/// A control against over-rejection: a grant added while the bind waited
/// does not make the recheck refuse it. This alone does not show that the
/// grants were resolved again, because a check against the admission-time
/// grants would also pass. The narrowed test above shows the grants are
/// resolved again, and the reowned test below shows the owner is.
#[tokio::test]
async fn channels_bind_widened_after_admission_is_honoured() {
    use zeroclaw_api::grants::{Resource, Verb};
    let tmp = tempfile::TempDir::new().unwrap();
    let (ctx, channels) = with_channels(binder_config(&tmp, &["test-agent"], &["peer_groups.*"]));
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;
    let response = bind_while_parked(&ctx, &mut peer, &mut rx, |config| {
        config
            .permission_profiles
            .get_mut("binder")
            .expect("the fixture profile exists")
            .grants
            .insert(Resource::Canvas, vec![Verb::Read]);
    })
    .await;
    assert_eq!(response["result"]["saved"], json!(true), "{response}");
    assert_eq!(
        *channels.calls.lock(),
        vec!["bind telegram.main 123456789".to_string()]
    );
}

/// The channel's owner is read again after the wait: a channel handed to an
/// agent the principal may not use while the bind waited is refused.
#[tokio::test]
async fn channels_bind_resource_reowned_after_admission_is_refused() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (ctx, channels) = with_channels(binder_config(&tmp, &["test-agent"], &["peer_groups.*"]));
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;
    let response = bind_while_parked(&ctx, &mut peer, &mut rx, |config| {
        config
            .agents
            .get_mut("test-agent")
            .unwrap()
            .channels
            .clear();
        config.agents.get_mut("beta").unwrap().channels = vec!["telegram.main".into()];
    })
    .await;
    assert_forbidden(&response, "a bind on a channel re-owned while it waited");
    assert!(
        channels.calls.lock().is_empty(),
        "nothing reached the bind after the channel changed hands"
    );
}

/// The write is authorized on the path it touches, as the dashboard route
/// authorizes it: a grant scoped to the channel's own peer group suffices,
/// and one scoped to another group does not.
#[tokio::test]
async fn channels_bind_authorizes_the_peer_group_it_writes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (ctx, channels) = with_channels(binder_config(
        &tmp,
        &["test-agent"],
        &["peer_groups.telegram_main.*"],
    ));
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;
    let bound = rpc(
        &mut peer,
        &mut rx,
        1,
        "channels/bind",
        json!({"channel_type": "telegram", "alias": "main", "identity": "123456789"}),
    )
    .await;
    assert_eq!(bound["result"]["saved"], json!(true), "{bound}");

    let tmp = tempfile::TempDir::new().unwrap();
    let (ctx, other) = with_channels(binder_config(
        &tmp,
        &["test-agent"],
        &["peer_groups.telegram_other.*"],
    ));
    let (mut peer, mut rx) = roster_peer(&ctx, SCOPED).await;
    let refused = rpc(
        &mut peer,
        &mut rx,
        1,
        "channels/bind",
        json!({"channel_type": "telegram", "alias": "main", "identity": "123456789"}),
    )
    .await;
    assert_forbidden(&refused, "a grant scoped to another channel's group");
    assert_eq!(channels.calls.lock().len(), 1);
    assert!(other.calls.lock().is_empty());
}

// ── Authority at the effect: pairing ─────────────────────────────────────

/// A roster principal on a local peer credential whose only grant is
/// `admin`, so a policy change can take it away.
const PAIRING_ADMIN: u32 = 5104;

fn admin_pairing_config(tmp: &tempfile::TempDir) -> zeroclaw_config::schema::Config {
    use zeroclaw_config::schema::{PermissionProfileConfig, UserConfig};
    let mut config = pairing_config(tmp, true);
    config.gateway.paired_tokens = vec![PAIRED_TOKEN.to_string()];
    config.permission_profiles.insert(
        "pairing-admin".into(),
        PermissionProfileConfig {
            admin: true,
            ..PermissionProfileConfig::default()
        },
    );
    config.users.insert(
        "pairing-admin".into(),
        UserConfig {
            uid: Some(PAIRING_ADMIN),
            permission_profiles: vec!["pairing-admin".into()],
            ..UserConfig::default()
        },
    );
    config
}

fn register_paired_device(ctx: &Arc<RpcContext>) {
    let now = chrono::Utc::now();
    crate::devices::DeviceRegistry::shared(&ctx.config.read().data_dir)
        .register(
            zeroclaw_config::pairing::PairingGuard::token_hash(PAIRED_TOKEN),
            crate::devices::DeviceInfo {
                id: "device-1".into(),
                name: Some("phone".into()),
                device_type: None,
                paired_at: now,
                last_seen: now,
                ip_address: None,
                capabilities: None,
            },
        )
        .unwrap();
}

/// Hold the config write lock, wait until `request` is queued on it (it has
/// cloned the lock's `Arc` to wait), then run `while_queued` and release.
/// Returns the request's response. Deterministic: no sleep decides whether
/// the request got there first.
async fn while_queued_on_the_config_lock(
    ctx: &Arc<RpcContext>,
    request: impl std::future::Future<Output = Value>,
    while_queued: impl FnOnce(),
) -> Value {
    let held = Arc::clone(&ctx.config_write_lock).lock_owned().await;
    let holders = Arc::strong_count(&ctx.config_write_lock);
    let queue_and_release = async {
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            while Arc::strong_count(&ctx.config_write_lock) <= holders {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the request reached the config write lock");
        while_queued();
        drop(held);
    };
    let (response, ()) = tokio::join!(request, queue_and_release);
    response
}

/// A rotation queued behind another config writer establishes the caller's
/// authority again once it holds the lock: an administrator grant withdrawn
/// while it waited stops it before any token is revoked or any code issued.
#[test]
fn pairing_rotation_after_admin_is_withdrawn_while_queued_issues_nothing() {
    // A rotation that is not stopped goes on to persist the pairing tokens,
    // whose config save exceeds the default debug test stack; see
    // `run_on_a_large_stack`.
    run_on_a_large_stack(|| async move {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = enforcement_ctx(admin_pairing_config(&tmp));
        register_paired_device(&ctx);
        let (mut admin, mut rx) = roster_peer(&ctx, PAIRING_ADMIN).await;

        let rotate = rpc(
            &mut admin,
            &mut rx,
            1,
            "pairing/new-code",
            json!({"rotate": "device-1"}),
        );
        let response = while_queued_on_the_config_lock(&ctx, rotate, || {
            let mut changed = ctx.config.read().clone();
            changed
                .permission_profiles
                .get_mut("pairing-admin")
                .expect("the fixture profile exists")
                .admin = false;
            *ctx.config.write() = changed.clone();
            let revision = ctx.auth.accepted_revision().saturating_add(1);
            ctx.auth
                .publish_accepted(&changed, revision)
                .expect("the narrowed policy publishes");
        })
        .await;

        assert_forbidden(
            &response,
            "a rotation whose admin grant was withdrawn while it waited",
        );
        assert!(
            !response.to_string().contains("pairing_code\":\""),
            "no code was issued: {response}"
        );
        assert!(
            ctx.auth.pairing().is_authenticated(PAIRED_TOKEN),
            "device-1's token was not revoked"
        );
    });
}

/// Revoking a device's credential takes the config write lock before the
/// token is dropped. So while a writer holds that lock, as `channels/bind`
/// does from its authority recheck through its commit, the credential it
/// rechecked stays valid: a revocation cannot land between that operation's
/// check and its effect, and takes effect once the lock is released.
#[test]
fn a_device_revocation_waits_for_a_config_writer_in_progress() {
    // The revocation persists the pairing tokens, whose config save exceeds
    // the default debug test stack; see `run_on_a_large_stack`.
    run_on_a_large_stack(|| async move {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = paired_ctx(&tmp);
        register_paired_device(&ctx);
        let (mut operator, mut rx) = local_operator(&ctx).await;

        let revoke = rpc(
            &mut operator,
            &mut rx,
            1,
            "pairing/revoke",
            json!({"device_id": "device-1"}),
        );
        let response = while_queued_on_the_config_lock(&ctx, revoke, || {
            assert!(
                ctx.auth.pairing().is_authenticated(PAIRED_TOKEN),
                "the credential was revoked while another writer held the config lock"
            );
        })
        .await;

        assert_eq!(
            response["result"]["device_id"],
            json!("device-1"),
            "{response}"
        );
        assert!(
            !ctx.auth.pairing().is_authenticated(PAIRED_TOKEN),
            "the revocation took effect once the lock was released"
        );
    });
}

// ── Authority at the effect: canvas and bind ─────────────────────────────

/// A non-admin principal that starts with access to every agent.
const WIDE: u32 = 5105;

fn wide_canvas_session_config(tmp: &tempfile::TempDir) -> zeroclaw_config::schema::Config {
    use std::collections::HashMap;
    use zeroclaw_api::grants::{Resource, Verb};
    use zeroclaw_config::schema::{PermissionProfileConfig, UserConfig};

    let mut config = make_acp_test_config(tmp);
    config.permission_profiles.insert(
        "wide".into(),
        PermissionProfileConfig {
            allowed_agents: vec!["*".into()],
            allowed_tools: vec!["canvas".into()],
            grants: HashMap::from([
                (
                    Resource::Sessions,
                    vec![Verb::Create, Verb::Read, Verb::Execute],
                ),
                (Resource::Tools, vec![Verb::Execute]),
            ]),
            ..PermissionProfileConfig::default()
        },
    );
    config.users.insert(
        "wide".into(),
        UserConfig {
            uid: Some(WIDE),
            permission_profiles: vec!["wide".into()],
            ..UserConfig::default()
        },
    );
    config
}

/// A session's canvas handle is bound to its agent, not to the grants its
/// principal held when it was built: a principal created with every-agent
/// access and then narrowed to one agent still cannot, through the session
/// it already has, read, overwrite or clear a canvas outside that agent's
/// namespace, whether the dashboard's or another agent's.
#[tokio::test]
async fn a_warm_session_narrowed_to_one_agent_cannot_reach_other_canvases() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(wide_canvas_session_config(&tmp));
    let (mut operator, mut op_rx) = local_operator(&ctx).await;
    for (id, (canvas_id, content)) in [
        ("default", "the dashboard's frame"),
        ("beta/default", "beta's frame"),
    ]
    .into_iter()
    .enumerate()
    {
        let seeded = rpc(
            &mut operator,
            &mut op_rx,
            id as u64 + 1,
            "canvas/render",
            json!({"canvas_id": canvas_id, "content_type": "text", "content": content}),
        )
        .await;
        assert!(seeded["error"].is_null(), "{seeded}");
    }

    let (mut peer, mut rx) = roster_peer(&ctx, WIDE).await;
    let created = rpc(
        &mut peer,
        &mut rx,
        1,
        "session/new",
        json!({"agent_alias": "test-agent", "session_id": "s-warm-canvas"}),
    )
    .await;
    assert_eq!(
        created["result"]["session_id"],
        json!("s-warm-canvas"),
        "{created}"
    );

    // Narrow the principal to one agent after the session exists.
    let mut narrowed = ctx.config.read().clone();
    narrowed
        .permission_profiles
        .get_mut("wide")
        .expect("the fixture profile exists")
        .allowed_agents = vec!["test-agent".into()];
    *ctx.config.write() = narrowed.clone();
    let revision = ctx.auth.accepted_revision().saturating_add(1);
    ctx.auth
        .publish_accepted(&narrowed, revision)
        .expect("the narrowed policy publishes");

    let agent = ctx
        .sessions
        .get_agent("s-warm-canvas")
        .await
        .expect("session exists");
    let agent = agent.lock().await;
    let run = |args: Value| {
        let agent = &agent;
        async move {
            agent
                .execute_tool_for_test("canvas", args)
                .await
                .expect("the session has the canvas tool")
                .expect("the canvas tool runs")
        }
    };
    for canvas_id in ["default", "beta/default"] {
        let snapshot = run(json!({"action": "snapshot", "canvas_id": canvas_id})).await;
        let shown = format!("{snapshot:?}");
        assert!(
            !shown.contains("dashboard's frame") && !shown.contains("beta's frame"),
            "{canvas_id} reached another canvas: {shown}"
        );
    }
    let rendered = run(json!({
        "action": "render",
        "canvas_id": "default",
        "content_type": "text",
        "content": "drawn after narrowing",
    }))
    .await;
    assert!(rendered.success, "{rendered:?}");
    let _ = run(json!({"action": "clear", "canvas_id": "beta/default"})).await;
    drop(agent);

    for (id, (canvas_id, expected)) in [
        ("default", "the dashboard's frame"),
        ("beta/default", "beta's frame"),
        ("test-agent/default", "drawn after narrowing"),
    ]
    .into_iter()
    .enumerate()
    {
        let got = rpc(
            &mut operator,
            &mut op_rx,
            id as u64 + 10,
            "canvas/get",
            json!({"canvas_id": canvas_id}),
        )
        .await;
        assert_eq!(
            got["result"]["frame"]["content"],
            json!(expected),
            "{canvas_id}: {got}"
        );
    }
}

/// An alias the config validator would refuse can still arrive through a
/// hand-written `[agents."alpha/beta"]` table. Namespaced, it would share keys
/// with `alpha`: `alpha` drawing `beta/default` and `alpha/beta` drawing
/// `default` would be the one canvas `alpha/beta/default`. The session on
/// `alpha/beta` gets a private store instead, and the shared store holds and
/// lists only what `alpha` drew.
#[tokio::test]
async fn an_agent_alias_with_a_separator_shares_no_canvas_with_its_prefix() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut config = make_acp_test_config(&tmp);
    let agent = config.agents["test-agent"].clone();
    config.agents.insert("alpha".into(), agent.clone());
    config.agents.insert("alpha/beta".into(), agent);
    let ctx = enforcement_ctx(config);
    // Each session is created on a connection of its own, and both stay open
    // while the canvases are drawn.
    let mut connections = Vec::new();
    for (alias, session) in [("alpha", "s-alpha"), ("alpha/beta", "s-alpha-beta")] {
        let (mut peer, mut peer_rx) = local_operator(&ctx).await;
        let created = rpc(
            &mut peer,
            &mut peer_rx,
            1,
            "session/new",
            json!({"agent_alias": alias, "session_id": session}),
        )
        .await;
        assert_eq!(created["result"]["session_id"], json!(session), "{created}");
        connections.push((peer, peer_rx));
    }
    let (mut operator, mut rx) = local_operator(&ctx).await;
    let canvas = |session: &'static str, args: Value| {
        let sessions = Arc::clone(&ctx.sessions);
        async move {
            let agent = sessions.get_agent(session).await.expect("session exists");
            let agent = agent.lock().await;
            agent
                .execute_tool_for_test("canvas", args)
                .await
                .expect("the session has the canvas tool")
                .expect("the canvas tool runs")
        }
    };

    let drawn = canvas(
        "s-alpha",
        json!({"action": "render", "canvas_id": "beta/default",
               "content_type": "text", "content": "alpha's frame"}),
    )
    .await;
    assert!(drawn.success, "{drawn:?}");

    let seen = canvas(
        "s-alpha-beta",
        json!({"action": "snapshot", "canvas_id": "default"}),
    )
    .await;
    assert!(
        !format!("{seen:?}").contains("alpha's frame"),
        "alpha/beta read alpha's canvas: {seen:?}"
    );
    let overwritten = canvas(
        "s-alpha-beta",
        json!({"action": "render", "canvas_id": "default",
               "content_type": "text", "content": "alpha/beta's frame"}),
    )
    .await;
    assert!(overwritten.success, "{overwritten:?}");
    let _ = canvas(
        "s-alpha-beta",
        json!({"action": "clear", "canvas_id": "default"}),
    )
    .await;

    let got = rpc(
        &mut operator,
        &mut rx,
        10,
        "canvas/get",
        json!({"canvas_id": "alpha/beta/default"}),
    )
    .await;
    assert_eq!(
        got["result"]["frame"]["content"],
        json!("alpha's frame"),
        "{got}"
    );
    let listed = rpc(&mut operator, &mut rx, 11, "canvas/list", json!({})).await;
    assert_eq!(
        listed["result"]["canvases"],
        json!(["alpha/beta/default"]),
        "only alpha's canvas reaches the shared store: {listed}"
    );
}

/// A channel capability whose bind parks after the dispatcher has handed it
/// the held config write lock, at the point the real capability reads the
/// persisted peer policy, and records whether the caller's credential was
/// still valid when it went on to commit.
#[derive(Default)]
struct ParkingChannels {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    live_at_commit: parking_lot::Mutex<Option<bool>>,
    probe: parking_lot::Mutex<Option<Arc<dyn Fn() -> bool + Send + Sync>>>,
}

#[async_trait::async_trait]
impl crate::rpc::channels::ChannelControl for ParkingChannels {
    fn list(
        &self,
        _config: &zeroclaw_config::schema::Config,
        _pairing: &zeroclaw_config::pairing::PairingGuard,
    ) -> Value {
        json!({"channels": []})
    }

    fn relink(
        &self,
        _config: &zeroclaw_config::schema::Config,
        channel: &str,
    ) -> Result<Value, JsonRpcError> {
        Ok(json!({"channel": channel, "outcome": "nothing_to_clear"}))
    }

    async fn bind(
        &self,
        _config: &Arc<parking_lot::RwLock<zeroclaw_config::schema::Config>>,
        _config_write_guard: &tokio::sync::OwnedMutexGuard<()>,
        channel_type: &str,
        alias: &str,
        _identity: &str,
        authorize_write: &(
             dyn for<'p> Fn(&'p str, zeroclaw_api::grants::Verb) -> Result<(), JsonRpcError>
                 + Send
                 + Sync
         ),
    ) -> Result<Value, JsonRpcError> {
        self.entered.notify_one();
        self.release.notified().await;
        let probe = self
            .probe
            .lock()
            .clone()
            .expect("the test installs a probe");
        *self.live_at_commit.lock() = Some(probe());
        authorize_write(
            &format!("peer_groups.{channel_type}_{alias}.external_peers"),
            zeroclaw_api::grants::Verb::Create,
        )?;
        Ok(json!({"saved": true, "already_bound": false}))
    }
}

/// The caller's credential cannot be revoked between a bind's authority
/// recheck and its commit. The bind is parked inside its lock-held window;
/// an administrator's revocation of the binder's device is shown queued on
/// the config write lock with the binder's token still valid, the bind
/// commits with that token still valid, and only then does the revocation
/// take effect.
#[test]
fn a_revocation_cannot_land_inside_a_binds_authority_window() {
    // The revocation persists the pairing tokens, whose config save exceeds
    // the default debug test stack; see `run_on_a_large_stack`.
    run_on_a_large_stack(|| async move {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = binder_config(&tmp, &["*"], &["peer_groups.*"]);
        // No roster: a native pairing token binds as the shared operator.
        config.users.clear();
        config.data_dir = tmp.path().join("data");
        std::fs::create_dir_all(&config.data_dir).unwrap();
        config.gateway.require_pairing = true;
        config.gateway.paired_tokens = vec![PAIRED_TOKEN.to_string()];
        let mut ctx = enforcement_ctx(config);
        let channels = Arc::new(ParkingChannels::default());
        Arc::get_mut(&mut ctx)
            .expect("a fresh context is unshared")
            .channel_control =
            Some(Arc::clone(&channels) as Arc<dyn crate::rpc::channels::ChannelControl>);
        register_paired_device(&ctx);
        let pairing = Arc::clone(ctx.auth.pairing());
        *channels.probe.lock() = Some(Arc::new(move || pairing.is_authenticated(PAIRED_TOKEN)));

        // The binder authenticates with the device's own pairing token.
        let (tx, mut binder_rx) = tokio::sync::mpsc::channel(64);
        let mut binder = RpcDispatcher::new(Arc::clone(&ctx), tx, "wss:test".into())
            .with_transport(
                crate::rpc::transport::TransportKind::Wss,
                crate::security::auth_provider::Credential::None,
            );
        binder
            .handle_initialize(&json!({"auth_token": PAIRED_TOKEN}))
            .await
            .expect("the device's token authenticates");
        let (mut operator, mut op_rx) = local_operator(&ctx).await;

        let bind = rpc(
            &mut binder,
            &mut binder_rx,
            1,
            "channels/bind",
            json!({"channel_type": "telegram", "alias": "main", "identity": "123456789"}),
        );
        let revoke_inside_the_window = async {
            channels.entered.notified().await;
            // The bind is parked holding the config write lock.
            let holders = Arc::strong_count(&ctx.config_write_lock);
            let revoke = rpc(
                &mut operator,
                &mut op_rx,
                2,
                "pairing/revoke",
                json!({"device_id": "device-1"}),
            );
            let observe_then_release = async {
                tokio::time::timeout(std::time::Duration::from_secs(20), async {
                    while Arc::strong_count(&ctx.config_write_lock) <= holders {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                })
                .await
                .expect("the revocation reached the config write lock");
                assert!(
                    ctx.auth.pairing().is_authenticated(PAIRED_TOKEN),
                    "the binder's credential was revoked inside the bind's window"
                );
                channels.release.notify_one();
            };
            let (revoked, ()) = tokio::join!(revoke, observe_then_release);
            revoked
        };
        let (bound, revoked) = tokio::join!(bind, revoke_inside_the_window);

        assert_eq!(bound["result"]["saved"], json!(true), "{bound}");
        assert_eq!(
            *channels.live_at_commit.lock(),
            Some(true),
            "the bind committed with its caller's credential still valid"
        );
        assert_eq!(
            revoked["result"]["device_id"],
            json!("device-1"),
            "{revoked}"
        );
        assert!(
            !ctx.auth.pairing().is_authenticated(PAIRED_TOKEN),
            "the revocation took effect after the bind released the lock"
        );
    });
}
