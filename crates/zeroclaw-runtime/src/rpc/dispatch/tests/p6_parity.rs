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

/// The wildcard selector passes any alias string, so it is the principal an
/// alias that escapes `<install>/agents/` would have empowered.
#[tokio::test]
async fn a_wildcard_principal_cannot_escape_the_agents_tree_through_the_alias() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = enforcement_ctx(files_config(&tmp));
    let outside = tmp.path().join("elsewhere");
    std::fs::create_dir_all(outside.join("workspace")).unwrap();
    std::fs::write(outside.join("workspace/secret.md"), b"outside").unwrap();
    let (mut peer, mut rx) = roster_peer(&ctx, WILDCARD_UID).await;

    let absolute = outside.to_string_lossy().to_string();
    for (id, agent) in [(1, "../elsewhere"), (2, absolute.as_str()), (3, "..")] {
        let response = rpc(
            &mut peer,
            &mut rx,
            id,
            "fs/read",
            json!({"agent": agent, "path": "secret.md"}),
        )
        .await;
        assert_eq!(
            response["error"]["code"],
            json!(INVALID_PARAMS),
            "alias {agent:?}: {response}"
        );
    }
    assert!(outside.join("workspace/secret.md").exists());
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
/// agent's canvas tool wrote to a private store no reader could see.
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
        json!({"canvas_id": "board"}),
    )
    .await;
    assert_eq!(
        got["result"]["frame"]["content"],
        json!("drawn by the agent"),
        "{got}"
    );
    let listed = rpc(&mut operator, &mut rx, 3, "canvas/list", json!({})).await;
    assert_eq!(listed["result"]["canvases"], json!(["board"]), "{listed}");
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

#[tokio::test]
async fn an_administrator_manages_pairing_over_rpc() {
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
        _config: &Arc<parking_lot::RwLock<zeroclaw_config::schema::Config>>,
        _config_write_lock: &Arc<tokio::sync::Mutex<()>>,
        channel_type: &str,
        alias: &str,
        identity: &str,
    ) -> Result<Value, JsonRpcError> {
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

#[tokio::test]
async fn pairing_revoke_invalidates_a_registered_devices_token() {
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
}

#[tokio::test]
async fn pairing_revoke_all_invalidates_every_paired_token() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = paired_ctx(&tmp);
    let (mut operator, mut rx) = local_operator(&ctx).await;
    let rotated = rpc(&mut operator, &mut rx, 1, "pairing/revoke-all", json!({})).await;
    assert_eq!(rotated["result"]["success"], json!(true), "{rotated}");
    assert!(!ctx.auth.pairing().is_authenticated(PAIRED_TOKEN));
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
