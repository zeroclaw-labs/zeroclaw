#[cfg(test)]
use super::*;
use async_trait::async_trait;
use axum::http::{HeaderValue, Uri};
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use parking_lot::{Mutex, RwLock};
use std::sync::atomic::{AtomicUsize, Ordering};
use zeroclaw_memory::{Memory, MemoryCategory, MemoryEntry};
use zeroclaw_providers::ModelProvider;
use zeroclaw_runtime::agent::loop_::{mcp_tool_access_policy, register_eager_mcp_tool_if_allowed};

#[test]
fn default_agent_alias_picks_smallest_enabled_and_is_deterministic() {
    use zeroclaw_config::schema::AliasedAgentConfig;

    let enabled = || AliasedAgentConfig {
        enabled: true,
        ..AliasedAgentConfig::default()
    };

    // No agents -> no default.
    let mut config = Config::default();
    assert_eq!(default_agent_alias(&config), None);

    // Insertion order is deliberately not alphabetical; `config.agents` is
    // a HashMap whose iteration order is randomized per process. The pick
    // must still be the lexicographically smallest ENABLED alias so the
    // Tools page seeds the same agent on every restart.
    config.agents.insert("zeta".to_string(), enabled());
    config.agents.insert("alpha".to_string(), enabled());
    config.agents.insert("mid".to_string(), enabled());
    assert_eq!(default_agent_alias(&config).as_deref(), Some("alpha"));

    // A smaller-but-disabled alias is skipped (omission is not a grant).
    config.agents.insert(
        "aaa_disabled".to_string(),
        AliasedAgentConfig {
            enabled: false,
            ..AliasedAgentConfig::default()
        },
    );
    assert_eq!(default_agent_alias(&config).as_deref(), Some("alpha"));
}

/// Generate a random hex secret at runtime to avoid hard-coded cryptographic values.
fn generate_test_secret() -> String {
    let bytes: [u8; 32] = rand::random();
    hex::encode(bytes)
}

struct NamedMcpMockTool(&'static str);
zeroclaw_api::mock_tool_attribution!(NamedMcpMockTool);
#[async_trait]
impl tools::Tool for NamedMcpMockTool {
    fn name(&self) -> &str {
        self.0
    }
    fn description(&self) -> &str {
        "mcp mock"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<tools::ToolResult> {
        Ok(tools::ToolResult {
            success: true,
            output: tools::ToolOutput::default(),
            error: None,
        })
    }
}

#[test]
fn gateway_excluded_tools_drops_denied_mcp_tool() {
    let policy = SecurityPolicy {
        excluded_tools: Some(vec!["aa_mcp__find_items".to_string()]),
        workspace_dir: std::env::temp_dir(),
        ..SecurityPolicy::default()
    };
    let mcp_policy = mcp_tool_access_policy(&policy, None);
    let mut gw_tools: Vec<Box<dyn tools::Tool>> = Vec::new();
    let denied: std::sync::Arc<dyn tools::Tool> =
        std::sync::Arc::new(NamedMcpMockTool("aa_mcp__find_items"));
    let allowed: std::sync::Arc<dyn tools::Tool> =
        std::sync::Arc::new(NamedMcpMockTool("aa_mcp__find_npcs"));
    let registered_denied =
        register_eager_mcp_tool_if_allowed(denied, &mut gw_tools, mcp_policy.as_ref());
    let registered_allowed =
        register_eager_mcp_tool_if_allowed(allowed, &mut gw_tools, mcp_policy.as_ref());
    assert!(
        !registered_denied,
        "gateway must not register an `excluded_tools`-denied MCP tool"
    );
    assert!(
        registered_allowed,
        "gateway must register a non-denied MCP tool (allowlist auto-admit)"
    );
    let names: Vec<&str> = gw_tools.iter().map(|t| t.name()).collect();
    assert!(
        !names.contains(&"aa_mcp__find_items"),
        "denied MCP tool leaked into the gateway registry; got {names:?}"
    );
    assert!(
        names.contains(&"aa_mcp__find_npcs"),
        "allowed MCP tool missing from the gateway registry; got {names:?}"
    );
}

#[test]
fn security_body_limit_is_64kb() {
    assert_eq!(MAX_BODY_SIZE, 65_536);
}

#[test]
fn security_timeout_default_is_30_seconds() {
    assert_eq!(REQUEST_TIMEOUT_SECS, 30);
}

#[test]
fn gateway_timeout_uses_typed_config_default() {
    let cfg = zeroclaw_config::schema::GatewayConfig::default();
    assert_eq!(gateway_request_timeout_secs(&cfg), 30);
}

#[test]
fn paircode_recovery_command_includes_alternate_port() {
    assert_eq!(
        format_paircode_recovery_command("127.0.0.1", 42617),
        "zeroclaw gateway get-paircode --new --port 42617"
    );
}

#[test]
fn paircode_recovery_command_includes_specific_host_when_needed() {
    // Admin paircode routes are localhost-only, so the recovery hint must
    // not advertise a non-loopback `--host` (the admin guard would 403 it).
    // The CLI is left to fall back to its loopback default.
    assert_eq!(
        format_paircode_recovery_command("192.168.1.20", 42617),
        "zeroclaw gateway get-paircode --new --port 42617"
    );
}

#[test]
fn paircode_recovery_command_uses_loopback_for_nonloopback_host() {
    // a gateway bound to a non-loopback interface must
    // not surface a recovery hint that the localhost-only admin guard rejects.
    let cmd = format_paircode_recovery_command("192.168.1.20", 42617);
    assert!(
        !cmd.contains("192.168.1.20"),
        "recovery command must not advertise the non-loopback bound host: {cmd}"
    );
    assert!(
        !cmd.contains("--host"),
        "recovery command should omit --host so the CLI uses its loopback default: {cmd}"
    );

    let curl = format_paircode_recovery_curl(
        "192.168.1.20",
        42617,
        "",
        std::path::Path::new("/zc/data/gateway-admin.token"),
    );
    assert_eq!(
        curl,
        "curl --noproxy '*' -s -X POST -H \"x-zeroclaw-admin-token: $(cat '/zc/data/gateway-admin.token')\" http://127.0.0.1:42617/admin/paircode/new",
        "curl fallback must target loopback, not the non-loopback bound host"
    );
    assert!(
        !curl.contains("192.168.1.20"),
        "curl fallback must not advertise the non-loopback bound host: {curl}"
    );

    // Path prefix is still preserved while the host is normalized.
    assert_eq!(
        format_paircode_recovery_curl(
            "192.168.1.20",
            42617,
            "/gw",
            std::path::Path::new("/zc/data/gateway-admin.token")
        ),
        "curl --noproxy '*' -s -X POST -H \"x-zeroclaw-admin-token: $(cat '/zc/data/gateway-admin.token')\" http://127.0.0.1:42617/gw/admin/paircode/new"
    );
}

#[test]
fn paircode_recovery_curl_targets_running_instance() {
    assert_eq!(
        format_paircode_recovery_curl(
            "127.0.0.1",
            42617,
            "",
            std::path::Path::new("/zc/data/gateway-admin.token")
        ),
        "curl --noproxy '*' -s -X POST -H \"x-zeroclaw-admin-token: $(cat '/zc/data/gateway-admin.token')\" http://127.0.0.1:42617/admin/paircode/new"
    );
}

#[test]
fn already_paired_notice_states_no_code_was_generated() {
    // the banner must say plainly that NO code exists
    // (already paired), not just "Pairing: ACTIVE" — otherwise the operator
    // hits the dashboard's pairing-code prompt with no code printed
    // anywhere.
    let lines = already_paired_pairing_notice(
        "127.0.0.1",
        3001,
        "",
        std::path::Path::new("/zc/data/gateway-admin.token"),
    );
    let joined = lines.join("\n");
    assert!(
        joined.contains("already paired"),
        "notice must say the gateway is already paired: {joined}"
    );
    assert!(
        joined.contains("no new") && joined.contains("code"),
        "notice must state that no new code was generated: {joined}"
    );
}

#[test]
fn already_paired_notice_includes_recovery_command_and_curl() {
    // The notice is the single source of truth for the on-demand recovery
    // commands; it must reuse the loopback-safe builders so the banner and
    // any future surface never drift from's no-`--host` rule.
    let lines = already_paired_pairing_notice(
        "192.168.1.20",
        3001,
        "/gw",
        std::path::Path::new("/zc/data/gateway-admin.token"),
    );
    let joined = lines.join("\n");
    assert!(
        joined.contains(&format_paircode_recovery_command("192.168.1.20", 3001)),
        "notice must surface the get-paircode recovery command: {joined}"
    );
    assert!(
        joined.contains(&format_paircode_recovery_curl(
            "192.168.1.20",
            3001,
            "/gw",
            std::path::Path::new("/zc/data/gateway-admin.token")
        )),
        "notice must surface the curl fallback (honoring the path prefix): {joined}"
    );
    // never advertise the non-loopback bound host in the hint.
    assert!(
        !joined.contains("192.168.1.20"),
        "notice must not advertise the non-loopback bound host: {joined}"
    );
}

#[test]
fn paircode_recovery_curl_normalizes_unspecified_bind_hosts() {
    assert_eq!(
        format_paircode_recovery_curl(
            "0.0.0.0",
            42617,
            "",
            std::path::Path::new("/zc/data/gateway-admin.token")
        ),
        "curl --noproxy '*' -s -X POST -H \"x-zeroclaw-admin-token: $(cat '/zc/data/gateway-admin.token')\" http://127.0.0.1:42617/admin/paircode/new"
    );
    assert_eq!(
        format_paircode_recovery_curl(
            "::",
            42617,
            "",
            std::path::Path::new("/zc/data/gateway-admin.token")
        ),
        "curl --noproxy '*' -s -X POST -H \"x-zeroclaw-admin-token: $(cat '/zc/data/gateway-admin.token')\" http://127.0.0.1:42617/admin/paircode/new"
    );
}

#[test]
fn paircode_recovery_curl_preserves_actual_loopback_hosts() {
    assert_eq!(
        format_paircode_recovery_curl(
            "localhost",
            42617,
            "",
            std::path::Path::new("/zc/data/gateway-admin.token")
        ),
        "curl --noproxy '*' -s -X POST -H \"x-zeroclaw-admin-token: $(cat '/zc/data/gateway-admin.token')\" http://localhost:42617/admin/paircode/new"
    );
    assert_eq!(
        format_paircode_recovery_curl(
            "::1",
            42617,
            "",
            std::path::Path::new("/zc/data/gateway-admin.token")
        ),
        "curl --noproxy '*' -s -X POST -H \"x-zeroclaw-admin-token: $(cat '/zc/data/gateway-admin.token')\" http://[::1]:42617/admin/paircode/new"
    );
}

#[cfg(unix)]
#[test]
fn paircode_recovery_curl_quotes_admin_token_path() {
    let tmp = tempfile::tempdir().unwrap();
    let token_path = tmp.path().join("owner's $(touch escaped).token");
    std::fs::write(&token_path, "synthetic-admin-token").unwrap();
    let command = format_paircode_recovery_curl("127.0.0.1", 42617, "", &token_path);
    // Intercept curl: exercise actual shell parsing and cat without a
    // network call, and verify path contents cannot become shell syntax.
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("curl() {{ printf '%s\\n' \"$@\"; }}; {command}"))
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let args = String::from_utf8(output.stdout).unwrap();
    assert!(
        args.lines()
            .any(|arg| arg == "x-zeroclaw-admin-token: synthetic-admin-token"),
        "{args}"
    );
    assert!(!tmp.path().join("escaped").exists());
}

#[cfg(unix)]
#[test]
fn paircode_recovery_curl_bypasses_proxy_with_actual_command() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    let tmp = tempfile::tempdir().unwrap();
    let token_path = tmp.path().join("mock-owner.token");
    std::fs::write(&token_path, "local-mock-owner-token").unwrap();
    let origin = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = origin.local_addr().unwrap().port();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    origin.set_nonblocking(true).unwrap();
    proxy.set_nonblocking(true).unwrap();
    let command = format_paircode_recovery_curl("127.0.0.1", port, "/gw", &token_path);
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            for (is_proxy, listener) in [(false, &origin), (true, &proxy)] {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        socket
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut request = Vec::new();
                        let mut chunk = [0; 1024];
                        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                            let count = socket.read(&mut chunk).unwrap();
                            assert!(count > 0, "request ended before headers");
                            request.extend_from_slice(&chunk[..count]);
                        }
                        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").unwrap();
                        return (is_proxy, String::from_utf8(request).unwrap());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) => panic!("mock accept failed: {error}"),
                }
            }
            assert!(
                Instant::now() < deadline,
                "generated curl reached neither local mock"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    // Execute the production-generated shell command unchanged. Every proxy
    // setting points to a local mock; no real credentials or external peers.
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .env("CURL_HOME", tmp.path())
        .env("http_proxy", &proxy_url)
        .env("HTTP_PROXY", &proxy_url)
        .env("https_proxy", &proxy_url)
        .env("HTTPS_PROXY", &proxy_url)
        .env("all_proxy", &proxy_url)
        .env("ALL_PROXY", &proxy_url)
        .env("no_proxy", "")
        .env("NO_PROXY", "")
        .output()
        .unwrap();
    let (reached_proxy, request) = server.join().unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    assert!(
        !reached_proxy,
        "generated curl forwarded the owner header to the proxy"
    );
    assert!(request.starts_with("POST /gw/admin/paircode/new HTTP/1.1\r\n"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("\r\nx-zeroclaw-admin-token: local-mock-owner-token\r\n")
    );
}

#[test]
fn paircode_recovery_curl_preserves_path_prefix() {
    assert_eq!(
        format_paircode_recovery_curl(
            "127.0.0.1",
            42617,
            "/gw",
            std::path::Path::new("/zc/data/gateway-admin.token")
        ),
        "curl --noproxy '*' -s -X POST -H \"x-zeroclaw-admin-token: $(cat '/zc/data/gateway-admin.token')\" http://127.0.0.1:42617/gw/admin/paircode/new"
    );
}

/// Build an AppState wired with a real pairing guard, on-disk config path,
/// and an optional device registry so the admin paircode handler's
/// revoke + persist paths can be exercised end to end.
pub(crate) fn admin_paircode_state(
    tmp: &tempfile::TempDir,
    require_pairing: bool,
    with_registry: bool,
) -> AppState {
    let data_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = Config {
        data_dir: data_dir.clone(),
        config_path: tmp.path().join("config.toml"),
        ..Config::default()
    };
    let registry = with_registry.then(|| Arc::new(api_pairing::DeviceRegistry::new(&data_dir)));
    AppState {
        config: Arc::new(RwLock::new(config)),
        config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
        model_provider: Arc::new(MockModelProvider::default()),
        model: "test-model".into(),
        temperature: None,
        mem: Arc::new(MockMemory),
        memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
            Arc::new(MockMemory),
            zeroclaw_config::schema::MemoryConfig::default(),
            std::path::PathBuf::new(),
        )),
        companion_store: None,
        auto_save: false,
        webhook_secret_hash: None,
        pairing: Arc::new(PairingGuard::new(require_pairing, &[])),
        trust_forwarded_headers: false,
        rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
        auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
        idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
        observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
        tools_registry: Arc::new(Vec::new()),
        tools_registry_by_agent: Arc::new(std::collections::HashMap::new()),
        cost_tracker: None,
        event_tx: tokio::sync::broadcast::channel(16).0,
        event_buffer: Arc::new(sse::EventBuffer::new(16)),
        shutdown_tx: tokio::sync::watch::channel(false).0,
        reload_tx: None,
        #[cfg(feature = "nodes")]
        node_registry: Arc::new(nodes::NodeRegistry::new(16)),
        #[cfg(feature = "nodes")]
        mdns_peer_registry: nodes::mdns::MdnsPeerRegistry::default(),
        path_prefix: String::new(),
        web_dist_dir: None,
        session_backend: None,
        session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
            8, 30, 600,
        )),
        device_registry: registry,
        pending_pairings: None,
        cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ws_conversations: Default::default(),
        bridge_sockets: Default::default(),
    }
}

fn spa_fallback_state(tmp: &tempfile::TempDir) -> AppState {
    let dist_dir = tmp.path().join("web").join("dist");
    std::fs::create_dir_all(&dist_dir).unwrap();
    std::fs::write(
        dist_dir.join("index.html"),
        r#"<!DOCTYPE html><html><head></head><body>dashboard shell</body></html>"#,
    )
    .unwrap();

    let mut state = admin_paircode_state(tmp, false, false);
    state.web_dist_dir = Some(dist_dir);
    state
}

async fn spa_fallback_response(path: &'static str, state: AppState) -> axum::response::Response {
    static_files::handle_spa_fallback(State(state), Uri::from_static(path)).await
}

/// Pair a device into both the pairing guard and the device registry,
/// returning the plaintext token so the test can assert it is revoked.
async fn pair_device(state: &AppState, device_id: &str) -> String {
    let code = state
        .pairing
        .generate_new_pairing_code()
        .expect("pairing enabled");
    let token = state
        .pairing
        .try_pair(&code, device_id)
        .await
        .unwrap()
        .unwrap();
    state
        .device_registry
        .as_ref()
        .unwrap()
        .register(
            PairingGuard::token_hash(&token),
            api_pairing::DeviceInfo {
                id: device_id.to_string(),
                name: None,
                device_type: None,
                paired_at: chrono::Utc::now(),
                last_seen: chrono::Utc::now(),
                ip_address: None,
                capabilities: None,
            },
        )
        .expect("test device registry insert");
    token
}

async fn admin_paircode_response_json(
    result: Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)>,
) -> (StatusCode, serde_json::Value) {
    let response = result.into_response();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    (status, json)
}

#[tokio::test]
async fn admin_paircode_new_without_rotate_keeps_existing_tokens() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = admin_paircode_state(&tmp, true, true);
    let token = pair_device(&state, "dev-a").await;

    let (status, json) = admin_paircode_response_json(
        handle_admin_paircode_new(
            State(state.clone()),
            test_connect_info(),
            admin_headers(&state),
            Query(AdminPaircodeQuery::default()),
        )
        .await,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(json["pairing_code"].is_string());
    assert!(
        state.pairing.is_authenticated(&token),
        "add-another-client path must not revoke existing tokens"
    );
}

#[tokio::test]
async fn admin_paircode_new_rotate_all_revokes_everything() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = admin_paircode_state(&tmp, true, true);
    let token_a = pair_device(&state, "dev-a").await;
    let token_b = pair_device(&state, "dev-b").await;

    let (status, json) = admin_paircode_response_json(
        handle_admin_paircode_new(
            State(state.clone()),
            test_connect_info(),
            admin_headers(&state),
            Query(AdminPaircodeQuery {
                rotate: Some("all".into()),
            }),
        )
        .await,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(json["pairing_code"].is_string());
    assert!(!state.pairing.is_authenticated(&token_a));
    assert!(!state.pairing.is_authenticated(&token_b));
    assert!(
        state.config.read().gateway.paired_tokens.is_empty(),
        "rotate=all must persist an empty token set"
    );
    assert!(
        state
            .device_registry
            .as_ref()
            .unwrap()
            .list()
            .expect("test device registry list")
            .is_empty(),
        "rotate=all must clear the device registry"
    );
}

#[tokio::test]
async fn admin_paircode_new_rotate_device_revokes_one() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = admin_paircode_state(&tmp, true, true);
    let token_a = pair_device(&state, "dev-a").await;
    let token_b = pair_device(&state, "dev-b").await;

    let (status, json) = admin_paircode_response_json(
        handle_admin_paircode_new(
            State(state.clone()),
            test_connect_info(),
            admin_headers(&state),
            Query(AdminPaircodeQuery {
                rotate: Some("dev-a".into()),
            }),
        )
        .await,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(json["pairing_code"].is_string());
    assert!(!state.pairing.is_authenticated(&token_a));
    assert!(
        state.pairing.is_authenticated(&token_b),
        "targeted rotate must not touch other devices"
    );
    let old_hash = PairingGuard::token_hash(&token_a);
    assert!(
        !state
            .config
            .read()
            .gateway
            .paired_tokens
            .contains(&old_hash)
    );
}

#[tokio::test]
async fn admin_paircode_new_rotate_unknown_device_is_not_found() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = admin_paircode_state(&tmp, true, true);
    let token = pair_device(&state, "dev-a").await;

    let (status, _json) = admin_paircode_response_json(
        handle_admin_paircode_new(
            State(state.clone()),
            test_connect_info(),
            admin_headers(&state),
            Query(AdminPaircodeQuery {
                rotate: Some("ghost".into()),
            }),
        )
        .await,
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        state.pairing.is_authenticated(&token),
        "a not-found rotate must not revoke any token"
    );
}

#[tokio::test]
async fn admin_paircode_new_pairing_disabled_is_bad_request() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = admin_paircode_state(&tmp, false, false);

    let (status, json) = admin_paircode_response_json(
        handle_admin_paircode_new(
            State(state.clone()),
            test_connect_info(),
            admin_headers(&state),
            Query(AdminPaircodeQuery {
                rotate: Some("all".into()),
            }),
        )
        .await,
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["success"], false);
}

#[tokio::test]
async fn admin_paircode_new_rejects_remote_peer() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = admin_paircode_state(&tmp, true, true);

    let remote = ConnectInfo(SocketAddr::from(([203, 0, 113, 7], 40_000)));
    let (status, _json) = admin_paircode_response_json(
        handle_admin_paircode_new(
            State(state.clone()),
            remote,
            admin_headers(&state),
            Query(AdminPaircodeQuery::default()),
        )
        .await,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "minting a pairing code must be rejected for non-loopback peers"
    );
}

#[test]
fn long_running_request_timeout_default_is_ten_minutes() {
    assert_eq!(LONG_RUNNING_REQUEST_TIMEOUT_SECS, 600);
}

#[test]
fn long_running_request_timeout_uses_typed_config_default() {
    let cfg = zeroclaw_config::schema::GatewayConfig::default();
    assert_eq!(gateway_long_running_request_timeout_secs(&cfg), 600);
}

#[test]
fn webhook_body_requires_message_field() {
    let valid = r#"{"message": "hello"}"#;
    let parsed: Result<WebhookBody, _> = serde_json::from_str(valid);
    assert!(parsed.is_ok());
    assert_eq!(parsed.unwrap().message, "hello");

    let missing = r#"{"other": "field"}"#;
    let parsed: Result<WebhookBody, _> = serde_json::from_str(missing);
    assert!(parsed.is_err());
}

#[test]
fn app_state_is_clone() {
    fn assert_clone<T: Clone>() {}
    assert_clone::<AppState>();
}

#[tokio::test]
async fn spa_fallback_returns_json_not_html_for_unknown_api_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = spa_fallback_state(&tmp);

    let response = spa_fallback_response("/api/agents", state).await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/json")),
        "unknown API paths must not be served as HTML"
    );

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "not_found");
    assert_eq!(json["path"], "/api/agents");
}

#[tokio::test]
async fn spa_fallback_returns_json_for_api_root_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = spa_fallback_state(&tmp);

    let response = spa_fallback_response("/api", state).await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["path"], "/api");
}

#[tokio::test]
async fn spa_fallback_returns_json_for_path_prefixed_api_miss() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut state = spa_fallback_state(&tmp);
    state.path_prefix = "/gw".to_string();

    let response = spa_fallback_response("/gw/api/agents", state).await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["path"], "/api/agents");
}

#[tokio::test]
async fn spa_fallback_still_serves_dashboard_routes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = spa_fallback_state(&tmp);

    let response = spa_fallback_response("/config", state).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/html")),
        "dashboard routes should still receive the SPA shell"
    );

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("dashboard shell"));
}

#[tokio::test]
async fn spa_fallback_does_not_treat_api_like_spa_paths_as_api() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = spa_fallback_state(&tmp);

    let response = spa_fallback_response("/apiary", state).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/html")),
        "similarly named SPA routes should not be reserved as API paths"
    );
}

#[tokio::test]
async fn run_gateway_starts_with_zero_agents() {
    // Isolate data_dir so parallel nextest runs don't race on the
    // real ~/.zeroclaw/data
    let tmp = tempfile::TempDir::new().unwrap();
    let config = zeroclaw_config::schema::Config {
        data_dir: tmp.path().join("workspace"),
        config_path: tmp.path().join("config.toml"),
        ..zeroclaw_config::schema::Config::default()
    };
    std::fs::create_dir_all(&config.data_dir).unwrap();

    // Default Config has no [agents.*] entries — the exact shape
    // a fresh install presents on first daemon boot.
    assert!(
        config.agents.is_empty(),
        "regression assumes default Config has no agents",
    );

    let handle = zeroclaw_spawn::spawn!(async move {
        run_gateway("127.0.0.1", 0, config, None, None, None).await
    });

    match tokio::time::timeout(
        std::time::Duration::from_millis(750),
        &mut Box::pin(async {
            // We cannot await `handle` directly because the gateway
            // never returns under normal operation; instead, peek at
            // whether it has finished by polling join with a tiny
            // budget.
            let _ = tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }),
    )
    .await
    {
        Ok(()) => {}
        Err(_) => panic!("test setup timed out before checking gateway state"),
    }

    // If the boot path errored, the task is finished and join
    // returns the error. If it's still running, abort and accept
    // boot reached the serving stage.
    if handle.is_finished() {
        let result = handle.await.expect("task did not panic");
        panic!(
            "gateway exited during boot with zero agents — must stay up for reload: {:?}",
            result
        );
    }
    handle.abort();
}

#[tokio::test]
async fn run_gateway_starts_with_unresolved_agent_risk_profile() {
    use zeroclaw_config::schema::AliasedAgentConfig;

    // Isolate data_dir so parallel nextest runs don't race on the
    // real ~/.zeroclaw/data
    let tmp = tempfile::TempDir::new().unwrap();
    let mut config = zeroclaw_config::schema::Config {
        data_dir: tmp.path().join("workspace"),
        config_path: tmp.path().join("config.toml"),
        ..zeroclaw_config::schema::Config::default()
    };
    std::fs::create_dir_all(&config.data_dir).unwrap();

    // Enabled agent whose `risk_profile` does not resolve. No
    // matching [risk_profiles.<key>] entry exists.
    let agent = AliasedAgentConfig {
        enabled: true,
        risk_profile: "definitely_not_configured".into(),
        ..AliasedAgentConfig::default()
    };
    config.agents.insert("fake123".to_string(), agent);

    let handle = zeroclaw_spawn::spawn!(async move {
        run_gateway("127.0.0.1", 0, config, None, None, None).await
    });

    match tokio::time::timeout(
        std::time::Duration::from_millis(750),
        &mut Box::pin(async {
            let _ = tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }),
    )
    .await
    {
        Ok(()) => {}
        Err(_) => panic!("test setup timed out before checking gateway state"),
    }

    if handle.is_finished() {
        let result = handle.await.expect("task did not panic");
        panic!(
            "gateway exited during boot when agent.risk_profile was unresolved \
             — must stay up so operator can fix via /admin/reload: {:?}",
            result
        );
    }
    handle.abort();
}

#[tokio::test]
async fn run_gateway_starts_with_mismatched_provider_api_key() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut config = Config {
        data_dir: tmp.path().join("workspace"),
        config_path: tmp.path().join("config.toml"),
        ..Config::default()
    };
    config.providers.models.anthropic.insert(
        "default".to_string(),
        zeroclaw_config::schema::AnthropicModelProviderConfig {
            base: zeroclaw_config::schema::ModelProviderConfig {
                model: Some("anthropic/claude-sonnet-4-6".to_string()),
                api_key: Some("sk-test-openai-shaped-key".to_string()),
                ..Default::default()
            },
        },
    );

    let handle = zeroclaw_spawn::spawn!(async move {
        run_gateway("127.0.0.1", 0, config, None, None, None).await
    });

    match tokio::time::timeout(
        std::time::Duration::from_millis(750),
        &mut Box::pin(async {
            let _ = tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }),
    )
    .await
    {
        Ok(()) => {}
        Err(_) => panic!("test setup timed out before checking gateway state"),
    }

    if handle.is_finished() {
        let result = handle.await.expect("task did not panic");
        panic!(
            "gateway exited during boot when seed provider API key was \
             mismatched — must stay up so operator can fix via /admin/reload: {:?}",
            result
        );
    }
    handle.abort();
}

#[tokio::test]
async fn run_gateway_uses_external_shutdown_sender() {
    let port_probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = port_probe.local_addr().unwrap().port();
    drop(port_probe);

    let tmp = tempfile::TempDir::new().unwrap();
    let config = zeroclaw_config::schema::Config {
        data_dir: tmp.path().join("workspace"),
        config_path: tmp.path().join("config.toml"),
        ..zeroclaw_config::schema::Config::default()
    };
    std::fs::create_dir_all(&config.data_dir).unwrap();

    let (shutdown_tx, _) = tokio::sync::watch::channel(false);
    let (reload_tx, _) = tokio::sync::watch::channel(false);
    let reload_controls = zeroclaw_runtime::daemon::GatewayReloadControls {
        shutdown_tx: shutdown_tx.clone(),
        reload_tx,
    };

    let handle = zeroclaw_spawn::spawn!(async move {
        run_gateway("127.0.0.1", port, config, None, Some(reload_controls), None).await
    });

    let addr = format!("127.0.0.1:{port}");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if tokio::net::TcpStream::connect(&addr).await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("gateway should accept connections before shutdown");

    shutdown_tx
        .send(true)
        .expect("external daemon-owned shutdown sender should stay connected");

    tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("gateway should return after external shutdown")
        .expect("gateway task should not panic")
        .expect("gateway shutdown should be graceful");

    std::net::TcpListener::bind(("127.0.0.1", port))
        .expect("gateway should release the listener after external shutdown");
}

#[tokio::test]
async fn metrics_endpoint_returns_hint_when_prometheus_is_disabled() {
    let state = AppState {
        config: Arc::new(RwLock::new(Config::default())),
        config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
        model_provider: Arc::new(MockModelProvider::default()),
        model: "test-model".into(),
        temperature: None,
        mem: Arc::new(MockMemory),
        memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
            Arc::new(MockMemory),
            zeroclaw_config::schema::MemoryConfig::default(),
            std::path::PathBuf::new(),
        )),
        companion_store: None,
        auto_save: false,
        webhook_secret_hash: None,
        pairing: Arc::new(PairingGuard::new(false, &[])),
        trust_forwarded_headers: false,
        rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
        auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
        idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
        observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
        tools_registry: Arc::new(Vec::new()),
        tools_registry_by_agent: Arc::new(std::collections::HashMap::new()),
        cost_tracker: None,
        event_tx: tokio::sync::broadcast::channel(16).0,
        event_buffer: Arc::new(sse::EventBuffer::new(16)),
        shutdown_tx: tokio::sync::watch::channel(false).0,
        reload_tx: None,
        #[cfg(feature = "nodes")]
        node_registry: Arc::new(nodes::NodeRegistry::new(16)),
        #[cfg(feature = "nodes")]
        mdns_peer_registry: nodes::mdns::MdnsPeerRegistry::default(),
        path_prefix: String::new(),
        web_dist_dir: None,
        session_backend: None,
        session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
            8, 30, 600,
        )),
        device_registry: None,
        pending_pairings: None,
        cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ws_conversations: Default::default(),
        bridge_sockets: Default::default(),
    };

    let response = handle_metrics(State(state)).await.into_response();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some(PROMETHEUS_CONTENT_TYPE)
    );

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("Prometheus backend not enabled"));
}

#[cfg(feature = "observability-prometheus")]
#[tokio::test]
async fn metrics_endpoint_renders_prometheus_output() {
    let event_tx = tokio::sync::broadcast::channel(16).0;
    let prom = zeroclaw_runtime::observability::PrometheusObserver::new();
    zeroclaw_runtime::observability::Observer::record_event(
        &prom,
        &zeroclaw_runtime::observability::ObserverEvent::HeartbeatTick,
    );

    let observer: Arc<dyn zeroclaw_runtime::observability::Observer> = Arc::new(prom);
    let state = AppState {
        config: Arc::new(RwLock::new(Config::default())),
        config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
        model_provider: Arc::new(MockModelProvider::default()),
        model: "test-model".into(),
        temperature: None,
        mem: Arc::new(MockMemory),
        memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
            Arc::new(MockMemory),
            zeroclaw_config::schema::MemoryConfig::default(),
            std::path::PathBuf::new(),
        )),
        companion_store: None,
        auto_save: false,
        webhook_secret_hash: None,
        pairing: Arc::new(PairingGuard::new(false, &[])),
        trust_forwarded_headers: false,
        rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
        auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
        idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
        observer,
        tools_registry: Arc::new(Vec::new()),
        tools_registry_by_agent: Arc::new(std::collections::HashMap::new()),
        cost_tracker: None,
        event_tx,
        event_buffer: Arc::new(sse::EventBuffer::new(16)),
        shutdown_tx: tokio::sync::watch::channel(false).0,
        reload_tx: None,
        #[cfg(feature = "nodes")]
        node_registry: Arc::new(nodes::NodeRegistry::new(16)),
        #[cfg(feature = "nodes")]
        mdns_peer_registry: nodes::mdns::MdnsPeerRegistry::default(),
        path_prefix: String::new(),
        web_dist_dir: None,
        session_backend: None,
        session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
            8, 30, 600,
        )),
        device_registry: None,
        pending_pairings: None,
        cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ws_conversations: Default::default(),
        bridge_sockets: Default::default(),
    };

    let response = handle_metrics(State(state)).await.into_response();
    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("zeroclaw_heartbeat_ticks_total 1"));
}

#[test]
fn gateway_rate_limiter_blocks_after_limit() {
    let limiter = GatewayRateLimiter::new(2, 2, 100);
    assert!(limiter.allow_pair("127.0.0.1"));
    assert!(limiter.allow_pair("127.0.0.1"));
    assert!(!limiter.allow_pair("127.0.0.1"));
}

#[test]
fn rate_limiter_sweep_removes_stale_entries() {
    let limiter = SlidingWindowRateLimiter::new(10, Duration::from_secs(60), 100);
    // Add entries for multiple IPs
    assert!(limiter.allow("ip-1"));
    assert!(limiter.allow("ip-2"));
    assert!(limiter.allow("ip-3"));

    {
        let guard = limiter.requests.lock();
        assert_eq!(guard.0.len(), 3);
    }

    // Force a sweep by backdating last_sweep
    {
        let mut guard = limiter.requests.lock();
        guard.1 = Instant::now()
            .checked_sub(Duration::from_secs(RATE_LIMITER_SWEEP_INTERVAL_SECS + 1))
            .unwrap();
        // Clear timestamps for ip-2 and ip-3 to simulate stale entries
        guard.0.get_mut("ip-2").unwrap().clear();
        guard.0.get_mut("ip-3").unwrap().clear();
    }

    // Next allow() call should trigger sweep and remove stale entries
    assert!(limiter.allow("ip-1"));

    {
        let guard = limiter.requests.lock();
        assert_eq!(guard.0.len(), 1, "Stale entries should have been swept");
        assert!(guard.0.contains_key("ip-1"));
    }
}

#[test]
fn rate_limiter_zero_limit_always_allows() {
    let limiter = SlidingWindowRateLimiter::new(0, Duration::from_secs(60), 10);
    for _ in 0..100 {
        assert!(limiter.allow("any-key"));
    }
}

#[test]
fn idempotency_store_rejects_duplicate_key() {
    let store = IdempotencyStore::new(Duration::from_secs(30), 10);
    assert!(store.record_if_new("req-1"));
    assert!(!store.record_if_new("req-1"));
    assert!(store.record_if_new("req-2"));
}

#[test]
fn rate_limiter_bounded_cardinality_evicts_oldest_key() {
    let limiter = SlidingWindowRateLimiter::new(5, Duration::from_secs(60), 2);
    assert!(limiter.allow("ip-1"));
    assert!(limiter.allow("ip-2"));
    assert!(limiter.allow("ip-3"));

    let guard = limiter.requests.lock();
    assert_eq!(guard.0.len(), 2);
    assert!(guard.0.contains_key("ip-2"));
    assert!(guard.0.contains_key("ip-3"));
}

#[test]
fn idempotency_store_bounded_cardinality_evicts_oldest_key() {
    let store = IdempotencyStore::new(Duration::from_secs(300), 2);
    assert!(store.record_if_new("k1"));
    std::thread::sleep(Duration::from_millis(2));
    assert!(store.record_if_new("k2"));
    std::thread::sleep(Duration::from_millis(2));
    assert!(store.record_if_new("k3"));

    let keys = store.keys.lock();
    assert_eq!(keys.len(), 2);
    assert!(!keys.contains_key("k1"));
    assert!(keys.contains_key("k2"));
    assert!(keys.contains_key("k3"));
}

#[test]
fn client_key_defaults_to_peer_addr_when_untrusted_proxy_mode() {
    let peer = SocketAddr::from(([10, 0, 0, 5], 42617));
    let mut headers = HeaderMap::new();
    headers.insert(
        "X-Forwarded-For",
        HeaderValue::from_static("198.51.100.10, 203.0.113.11"),
    );

    let key = client_key_from_request(Some(peer), &headers, false);
    assert_eq!(key, "10.0.0.5");
}

#[test]
fn client_key_uses_forwarded_ip_only_in_trusted_proxy_mode() {
    let peer = SocketAddr::from(([10, 0, 0, 5], 42617));
    let mut headers = HeaderMap::new();
    headers.insert(
        "X-Forwarded-For",
        HeaderValue::from_static("198.51.100.10, 203.0.113.11"),
    );

    let key = client_key_from_request(Some(peer), &headers, true);
    assert_eq!(key, "203.0.113.11");
}

#[test]
fn client_key_preserves_proxy_suffix_after_opaque_or_separate_prefix() {
    let peer = SocketAddr::from(([127, 0, 0, 1], 42617));
    for value in [&b"\x80, 198.51.100.10"[..], &b"garbage, 198.51.100.10"[..]] {
        let mut headers = HeaderMap::new();
        headers.insert("X-Forwarded-For", HeaderValue::from_bytes(value).unwrap());
        assert_eq!(
            client_key_from_request(Some(peer), &headers, true),
            "198.51.100.10"
        );
        assert_eq!(
            client_key_from_request(Some(peer), &headers, false),
            "127.0.0.1"
        );
    }
    let mut headers = HeaderMap::new();
    headers.append("X-Forwarded-For", HeaderValue::from_static("192.0.2.99"));
    headers.append("X-Forwarded-For", HeaderValue::from_static("198.51.100.10"));
    assert_eq!(
        client_key_from_request(Some(peer), &headers, true),
        "198.51.100.10"
    );
}

#[test]
fn client_key_forwarded_loopback_or_invalid_headers_never_gain_local_exemption() {
    let local = SocketAddr::from(([127, 0, 0, 1], 42617));
    let remote = SocketAddr::from(([203, 0, 113, 50], 42617));
    for (name, value) in [
        ("X-Forwarded-For", "127.0.0.1"),
        ("X-Forwarded-For", "::1, 127.0.0.1"),
        ("X-Forwarded-For", "invalid"),
        ("X-Forwarded-For", "198.51.100.10, invalid"),
        ("X-Forwarded-For", "198.51.100.10, 127.0.0.1"),
        ("X-Forwarded-For", "198.51.100.10, [::1]:8080"),
        ("X-Forwarded-For", "198.51.100.10, "),
        ("X-Real-IP", "127.0.0.1"),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_static(value));
        assert_eq!(
            client_key_from_request(Some(local), &headers, true),
            "unknown"
        );
        assert_eq!(
            client_key_from_request(Some(remote), &headers, true),
            "203.0.113.50"
        );
    }
    assert_eq!(
        client_key_from_request(Some(local), &HeaderMap::new(), true),
        "127.0.0.1"
    );
}

#[test]
fn client_key_invalid_terminal_field_does_not_fall_back_to_prefix_or_real_ip() {
    let peer = SocketAddr::from(([127, 0, 0, 1], 42617));
    for suffix in ["127.0.0.1", "invalid", ""] {
        let mut headers = HeaderMap::new();
        headers.append("X-Forwarded-For", HeaderValue::from_static("198.51.100.10"));
        headers.append("X-Forwarded-For", HeaderValue::from_static(suffix));
        headers.insert("X-Real-IP", HeaderValue::from_static("203.0.113.12"));
        assert_eq!(
            client_key_from_request(Some(peer), &headers, true),
            "unknown"
        );
    }
}

#[test]
fn client_key_handles_overwriting_proxy_and_real_ip_without_xff() {
    let peer = SocketAddr::from(([127, 0, 0, 1], 42617));
    for value in ["198.51.100.10", "\"198.51.100.10\"", "198.51.100.10:8080"] {
        let mut headers = HeaderMap::new();
        headers.insert("X-Forwarded-For", HeaderValue::from_static(value));
        assert_eq!(
            client_key_from_request(Some(peer), &headers, true),
            "198.51.100.10"
        );
    }
    let mut headers = HeaderMap::new();
    headers.insert("X-Real-IP", HeaderValue::from_static("198.51.100.10"));
    assert_eq!(
        client_key_from_request(Some(peer), &headers, true),
        "198.51.100.10"
    );
}

#[test]
fn client_key_falls_back_to_peer_when_forwarded_header_invalid() {
    let peer = SocketAddr::from(([10, 0, 0, 5], 42617));
    let mut headers = HeaderMap::new();
    headers.insert("X-Forwarded-For", HeaderValue::from_static("garbage-value"));

    let key = client_key_from_request(Some(peer), &headers, true);
    assert_eq!(key, "10.0.0.5");
}

#[test]
fn normalize_max_keys_uses_fallback_for_zero() {
    assert_eq!(normalize_max_keys(0, 10_000), 10_000);
    assert_eq!(normalize_max_keys(0, 0), 1);
}

#[test]
fn normalize_max_keys_preserves_nonzero_values() {
    assert_eq!(normalize_max_keys(2_048, 10_000), 2_048);
    assert_eq!(normalize_max_keys(1, 10_000), 1);
}

#[tokio::test]
async fn persist_pairing_tokens_writes_config_tokens() {
    let temp = tempfile::tempdir().unwrap();
    let config_path = temp.path().join("config.toml");
    let workspace_path = temp.path().join("workspace");

    let config = Config {
        config_path: config_path.clone(),
        data_dir: workspace_path,
        ..Default::default()
    };
    config.save().await.unwrap();

    let guard = PairingGuard::new(true, &[]);
    let code = guard.pairing_code().unwrap();
    let token = guard.try_pair(&code, "test_client").await.unwrap().unwrap();
    assert!(guard.is_authenticated(&token));

    let shared_config = Arc::new(RwLock::new(config));
    let config_write_lock = Arc::new(tokio::sync::Mutex::new(()));
    Box::pin(persist_pairing_tokens(
        shared_config.clone(),
        &guard,
        config_write_lock,
    ))
    .await
    .unwrap();

    // In-memory tokens should remain as plaintext 64-char hex hashes.
    let plaintext = {
        let in_memory = shared_config.read();
        assert_eq!(in_memory.gateway.paired_tokens.len(), 1);
        in_memory.gateway.paired_tokens[0].clone()
    };
    assert_eq!(plaintext.len(), 64);
    assert!(plaintext.chars().all(|c: char| c.is_ascii_hexdigit()));

    // On disk, the token should be encrypted (secrets.encrypt defaults to true).
    let saved = tokio::fs::read_to_string(config_path).await.unwrap();
    let raw_parsed: Config = toml::from_str(&saved).unwrap();
    assert_eq!(raw_parsed.gateway.paired_tokens.len(), 1);
    let on_disk = &raw_parsed.gateway.paired_tokens[0];
    assert!(
        zeroclaw_runtime::security::SecretStore::is_encrypted(on_disk),
        "paired_token should be encrypted on disk"
    );
}

/// Unlike the `persist_and_swap` callers (which pre-acquire the witness
/// before their own read-for-modify), `persist_pairing_tokens` acquires
/// `config_write_lock` internally since it is self-contained. This
/// proves that internal acquisition still serializes it against a
/// second, concurrent config mutation the same way. A single Pending
/// poll wouldn't distinguish "blocked on `config_write_lock`" from
/// "transiently Pending on unrelated I/O", so this polls repeatedly
/// with a no-op waker while the witness stays held and asserts the
/// future never completes -- proving it stays parked on the lock for as
/// long as it's held. Once the lock is released both changes land —
/// neither clobbers the other.
#[tokio::test]
async fn persist_pairing_tokens_serializes_against_concurrent_config_write() {
    let temp = tempfile::tempdir().unwrap();
    let config = Config {
        config_path: temp.path().join("config.toml"),
        data_dir: temp.path().join("workspace"),
        ..Default::default()
    };
    config.save().await.unwrap();

    let guard = PairingGuard::new(true, &[]);
    let code = guard.pairing_code().unwrap();
    let token = guard.try_pair(&code, "test_client").await.unwrap().unwrap();
    assert!(guard.is_authenticated(&token));

    let shared_config = Arc::new(RwLock::new(config));
    let config_write_lock = Arc::new(tokio::sync::Mutex::new(()));

    // Simulate another in-flight config mutation already holding the
    // witness for its own read-mutate-save-swap section.
    let held_guard = Arc::clone(&config_write_lock).lock_owned().await;

    let mut persist_fut = Box::pin(persist_pairing_tokens(
        shared_config.clone(),
        &guard,
        config_write_lock.clone(),
    ));

    // Bounded, sleep-free: `persist_pairing_tokens` acquires the witness
    // as its very first action, so poll with a no-op waker 50 times
    // while `held_guard` stays live and assert Pending every time,
    // rather than resolving synchronously or racing ahead after a
    // single yield.
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    for _ in 0..50 {
        assert!(
            std::future::Future::poll(persist_fut.as_mut(), &mut cx).is_pending(),
            "persist_pairing_tokens must stay parked on config_write_lock \
             acquisition for as long as another writer holds it"
        );
    }

    // Land a distinct, concurrent write directly on live config while
    // persist_pairing_tokens is parked waiting for the lock.
    shared_config.write().gateway.port = 55555;

    drop(held_guard);
    persist_fut
        .await
        .expect("persist_pairing_tokens must still succeed once unblocked");

    let live = shared_config.read();
    assert_eq!(
        live.gateway.port, 55555,
        "the concurrent writer's change must survive — no lost update"
    );
    assert_eq!(
        live.gateway.paired_tokens.len(),
        1,
        "persist_pairing_tokens' own token write must also land"
    );
}

#[test]
fn webhook_memory_key_is_unique() {
    let key1 = webhook_memory_key();
    let key2 = webhook_memory_key();

    assert!(key1.starts_with("webhook_msg_"));
    assert!(key2.starts_with("webhook_msg_"));
    assert_ne!(key1, key2);
}

#[test]
fn webhook_session_id_accepts_valid() {
    let mut headers = HeaderMap::new();
    headers.insert("X-Session-Id", HeaderValue::from_static("abc-DEF_123.foo"));
    assert_eq!(webhook_session_id(&headers), Some("abc-DEF_123.foo".into()));
}

#[test]
fn webhook_session_id_trims_whitespace() {
    let mut headers = HeaderMap::new();
    headers.insert("X-Session-Id", HeaderValue::from_static("  my-session  "));
    assert_eq!(webhook_session_id(&headers), Some("my-session".into()));
}

#[test]
fn webhook_session_id_rejects_empty() {
    let mut headers = HeaderMap::new();
    headers.insert("X-Session-Id", HeaderValue::from_static(""));
    assert_eq!(webhook_session_id(&headers), None);

    headers.insert("X-Session-Id", HeaderValue::from_static("   "));
    assert_eq!(webhook_session_id(&headers), None);
}

#[test]
fn webhook_session_id_rejects_missing() {
    let headers = HeaderMap::new();
    assert_eq!(webhook_session_id(&headers), None);
}

#[test]
fn webhook_session_id_rejects_oversized() {
    let mut headers = HeaderMap::new();
    let long = "a".repeat(129);
    headers.insert("X-Session-Id", HeaderValue::from_str(&long).unwrap());
    assert_eq!(webhook_session_id(&headers), None);

    let at_limit = "b".repeat(128);
    headers.insert("X-Session-Id", HeaderValue::from_str(&at_limit).unwrap());
    assert!(webhook_session_id(&headers).is_some());
}

#[test]
fn webhook_session_id_rejects_invalid_chars() {
    let mut headers = HeaderMap::new();
    for bad in &[
        "has/slash",
        "has:colon",
        "has space",
        "has@at",
        "emoji\u{1f600}",
    ] {
        if let Ok(val) = HeaderValue::from_str(bad) {
            headers.insert("X-Session-Id", val);
            assert_eq!(webhook_session_id(&headers), None, "should reject: {bad}");
        }
    }
}

#[derive(Default)]
struct MockMemory;

#[async_trait]
impl Memory for MockMemory {
    fn name(&self) -> &str {
        "mock"
    }

    async fn store(
        &self,
        _key: &str,
        _content: &str,
        _category: MemoryCategory,
        _session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn recall(
        &self,
        _query: &str,
        _limit: usize,
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }

    async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        Ok(None)
    }

    async fn list(
        &self,
        _category: Option<&MemoryCategory>,
        _session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }

    async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    async fn count(&self) -> anyhow::Result<usize> {
        Ok(0)
    }

    async fn health_check(&self) -> bool {
        true
    }

    async fn store_with_agent(
        &self,
        _key: &str,
        _content: &str,
        _category: MemoryCategory,
        _session_id: Option<&str>,
        _namespace: Option<&str>,
        _importance: Option<f64>,
        _agent_id: Option<&str>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn recall_for_agents(
        &self,
        _allowed_agent_ids: &[&str],
        _query: &str,
        _limit: usize,
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }
}
impl ::zeroclaw_api::attribution::Attributable for MockMemory {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Memory(::zeroclaw_api::attribution::MemoryKind::InMemory)
    }
    fn alias(&self) -> &str {
        "MockMemory"
    }
}

#[derive(Default)]
struct MockModelProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for MockModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok("ok".into())
    }
}
impl ::zeroclaw_api::attribution::Attributable for MockModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "MockModelProvider"
    }
}

#[derive(Default)]
struct CapturingObserver {
    events: Mutex<Vec<zeroclaw_runtime::observability::ObserverEvent>>,
}

impl zeroclaw_runtime::observability::Observer for CapturingObserver {
    fn record_event(&self, event: &zeroclaw_runtime::observability::ObserverEvent) {
        self.events.lock().push(event.clone());
    }

    fn record_metric(&self, _metric: &zeroclaw_runtime::observability::traits::ObserverMetric) {}

    fn name(&self) -> &str {
        "capturing"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[derive(Default)]
struct TrackingMemory {
    keys: Mutex<Vec<String>>,
}

#[async_trait]
impl Memory for TrackingMemory {
    fn name(&self) -> &str {
        "tracking"
    }

    async fn store(
        &self,
        key: &str,
        _content: &str,
        _category: MemoryCategory,
        _session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.keys.lock().push(key.to_string());
        Ok(())
    }

    async fn recall(
        &self,
        _query: &str,
        _limit: usize,
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }

    async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        Ok(None)
    }

    async fn list(
        &self,
        _category: Option<&MemoryCategory>,
        _session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }

    async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    async fn count(&self) -> anyhow::Result<usize> {
        let size = self.keys.lock().len();
        Ok(size)
    }

    async fn health_check(&self) -> bool {
        true
    }

    async fn store_with_agent(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        _namespace: Option<&str>,
        _importance: Option<f64>,
        _agent_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.store(key, content, category, session_id).await
    }

    async fn recall_for_agents(
        &self,
        _allowed_agent_ids: &[&str],
        _query: &str,
        _limit: usize,
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }
}
impl ::zeroclaw_api::attribution::Attributable for TrackingMemory {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Memory(::zeroclaw_api::attribution::MemoryKind::InMemory)
    }
    fn alias(&self) -> &str {
        "TrackingMemory"
    }
}

fn test_connect_info() -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 30_300)))
}

#[tokio::test]
async fn webhook_idempotency_skips_duplicate_provider_calls() {
    let provider_impl = Arc::new(MockModelProvider::default());
    let model_provider: Arc<dyn ModelProvider> = provider_impl.clone();
    let memory: Arc<dyn Memory> = Arc::new(MockMemory);

    let state = AppState {
        config: Arc::new(RwLock::new(Config::default())),
        config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
        model_provider,
        model: "test-model".into(),
        temperature: None,
        mem: memory.clone(),
        memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
            Arc::clone(&memory),
            zeroclaw_config::schema::MemoryConfig::default(),
            std::path::PathBuf::new(),
        )),
        companion_store: None,
        auto_save: false,
        webhook_secret_hash: None,
        pairing: Arc::new(PairingGuard::new(false, &[])),
        trust_forwarded_headers: false,
        rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
        auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
        idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
        observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
        tools_registry: Arc::new(Vec::new()),
        tools_registry_by_agent: Arc::new(std::collections::HashMap::new()),
        cost_tracker: None,
        event_tx: tokio::sync::broadcast::channel(16).0,
        event_buffer: Arc::new(sse::EventBuffer::new(16)),
        shutdown_tx: tokio::sync::watch::channel(false).0,
        reload_tx: None,
        #[cfg(feature = "nodes")]
        node_registry: Arc::new(nodes::NodeRegistry::new(16)),
        #[cfg(feature = "nodes")]
        mdns_peer_registry: nodes::mdns::MdnsPeerRegistry::default(),
        path_prefix: String::new(),
        web_dist_dir: None,
        session_backend: None,
        session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
            8, 30, 600,
        )),
        device_registry: None,
        pending_pairings: None,
        cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ws_conversations: Default::default(),
        bridge_sockets: Default::default(),
    };

    let mut headers = HeaderMap::new();
    headers.insert("X-Idempotency-Key", HeaderValue::from_static("abc-123"));

    let body = Ok(Json(WebhookBody {
        message: "hello".into(),
    }));
    let first = handle_webhook(
        State(state.clone()),
        test_connect_info(),
        Query(WebhookQuery::default()),
        headers.clone(),
        body,
    )
    .await
    .into_response();
    assert_eq!(first.status(), StatusCode::OK);

    let body = Ok(Json(WebhookBody {
        message: "hello".into(),
    }));
    let second = handle_webhook(
        State(state),
        test_connect_info(),
        Query(WebhookQuery::default()),
        headers,
        body,
    )
    .await
    .into_response();
    assert_eq!(second.status(), StatusCode::OK);

    let payload = second.into_body().collect().await.unwrap().to_bytes();
    let parsed: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(parsed["status"], "duplicate");
    assert_eq!(parsed["idempotent"], true);
    assert_eq!(provider_impl.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn webhook_unknown_agent_rejected_before_dispatch() {
    let provider_impl = Arc::new(MockModelProvider::default());
    let model_provider: Arc<dyn ModelProvider> = provider_impl.clone();
    let memory: Arc<dyn Memory> = Arc::new(MockMemory);

    let state = AppState {
        config: Arc::new(RwLock::new(Config::default())),
        config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
        model_provider,
        model: "test-model".into(),
        temperature: None,
        mem: memory.clone(),
        memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
            Arc::clone(&memory),
            zeroclaw_config::schema::MemoryConfig::default(),
            std::path::PathBuf::new(),
        )),
        companion_store: None,
        auto_save: false,
        webhook_secret_hash: None,
        pairing: Arc::new(PairingGuard::new(false, &[])),
        trust_forwarded_headers: false,
        rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
        auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
        idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
        observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
        tools_registry: Arc::new(Vec::new()),
        tools_registry_by_agent: Arc::new(std::collections::HashMap::new()),
        cost_tracker: None,
        event_tx: tokio::sync::broadcast::channel(16).0,
        event_buffer: Arc::new(sse::EventBuffer::new(16)),
        shutdown_tx: tokio::sync::watch::channel(false).0,
        reload_tx: None,
        #[cfg(feature = "nodes")]
        node_registry: Arc::new(nodes::NodeRegistry::new(16)),
        #[cfg(feature = "nodes")]
        mdns_peer_registry: nodes::mdns::MdnsPeerRegistry::default(),
        path_prefix: String::new(),
        web_dist_dir: None,
        session_backend: None,
        session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
            8, 30, 600,
        )),
        device_registry: None,
        pending_pairings: None,
        cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ws_conversations: Default::default(),
        bridge_sockets: Default::default(),
    };

    // An idempotency key on a rejected request must NOT be consumed.
    let mut headers = HeaderMap::new();
    headers.insert("X-Idempotency-Key", HeaderValue::from_static("ghost-key"));

    let response = handle_webhook(
        State(state.clone()),
        test_connect_info(),
        Query(WebhookQuery {
            agent: Some("ghost".into()),
        }),
        headers,
        Ok(Json(WebhookBody {
            message: "hello".into(),
        })),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload = response.into_body().collect().await.unwrap().to_bytes();
    let parsed: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert!(
        parsed["error"]
            .as_str()
            .unwrap_or_default()
            .contains("Unknown agent `ghost`")
    );
    assert_eq!(provider_impl.calls.load(Ordering::SeqCst), 0);
    // Key still fresh — a corrected retry with the same key proceeds.
    assert!(state.idempotency_store.record_if_new("ghost-key"));
}

#[tokio::test]
async fn webhook_explicit_agent_reports_model_without_owning_lifecycle() {
    let provider_impl = Arc::new(MockModelProvider::default());
    let model_provider: Arc<dyn ModelProvider> = provider_impl.clone();
    let memory: Arc<dyn Memory> = Arc::new(MockMemory);
    let observer_impl = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn zeroclaw_runtime::observability::Observer> = observer_impl.clone();

    let mut config = Config::default();
    config.providers.models.anthropic.insert(
        "default".into(),
        zeroclaw_config::schema::AnthropicModelProviderConfig {
            base: zeroclaw_config::schema::ModelProviderConfig {
                model: Some("agent-model".into()),
                ..Default::default()
            },
        },
    );
    let expected_provider = "anthropic.default".to_string();
    config.agents.insert(
        "nova".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: true,
            model_provider: expected_provider.clone().into(),
            ..Default::default()
        },
    );

    let state = AppState {
        config: Arc::new(RwLock::new(config)),
        config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
        model_provider,
        model: "startup-model".into(),
        temperature: None,
        mem: memory,
        memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
            Arc::new(MockMemory),
            zeroclaw_config::schema::MemoryConfig::default(),
            std::path::PathBuf::new(),
        )),
        companion_store: None,
        auto_save: false,
        webhook_secret_hash: None,
        pairing: Arc::new(PairingGuard::new(false, &[])),
        trust_forwarded_headers: false,
        rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
        auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
        idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
        observer,
        tools_registry: Arc::new(Vec::new()),
        tools_registry_by_agent: Arc::new(std::collections::HashMap::new()),
        cost_tracker: None,
        event_tx: tokio::sync::broadcast::channel(16).0,
        event_buffer: Arc::new(sse::EventBuffer::new(16)),
        shutdown_tx: tokio::sync::watch::channel(false).0,
        reload_tx: None,
        #[cfg(feature = "nodes")]
        node_registry: Arc::new(nodes::NodeRegistry::new(16)),
        #[cfg(feature = "nodes")]
        mdns_peer_registry: nodes::mdns::MdnsPeerRegistry::default(),
        path_prefix: String::new(),
        web_dist_dir: None,
        session_backend: None,
        session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
            8, 30, 600,
        )),
        device_registry: None,
        pending_pairings: None,
        cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ws_conversations: Default::default(),
        bridge_sockets: Default::default(),
    };

    let response = handle_webhook(
        State(state),
        test_connect_info(),
        Query(WebhookQuery {
            agent: Some("nova".into()),
        }),
        HeaderMap::new(),
        Ok(Json(WebhookBody {
            message: "hello".into(),
        })),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(provider_impl.calls.load(Ordering::SeqCst), 1);
    let payload = response.into_body().collect().await.unwrap().to_bytes();
    let parsed: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(parsed["model"], "agent-model");
    let events = observer_impl.events.lock();
    assert!(
        !events.iter().any(|event| matches!(
            event,
            zeroclaw_runtime::observability::ObserverEvent::AgentStart { .. }
                | zeroclaw_runtime::observability::ObserverEvent::AgentEnd { .. }
                | zeroclaw_runtime::observability::ObserverEvent::LlmRequest { .. }
                | zeroclaw_runtime::observability::ObserverEvent::LlmResponse { .. }
        )),
        "the HTTP handler must not create a second agent lifecycle; events were: {events:?}"
    );
}

#[tokio::test]
async fn webhook_autosave_stores_distinct_keys_per_request() {
    let provider_impl = Arc::new(MockModelProvider::default());
    let model_provider: Arc<dyn ModelProvider> = provider_impl.clone();

    let tracking_impl = Arc::new(TrackingMemory::default());
    let memory: Arc<dyn Memory> = tracking_impl.clone();

    let state = AppState {
        config: Arc::new(RwLock::new(Config::default())),
        config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
        model_provider,
        model: "test-model".into(),
        temperature: None,
        mem: memory,
        memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
            Arc::new(MockMemory),
            zeroclaw_config::schema::MemoryConfig::default(),
            std::path::PathBuf::new(),
        )),
        companion_store: None,
        auto_save: true,
        webhook_secret_hash: None,
        pairing: Arc::new(PairingGuard::new(false, &[])),
        trust_forwarded_headers: false,
        rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
        auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
        idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
        observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
        tools_registry: Arc::new(Vec::new()),
        tools_registry_by_agent: Arc::new(std::collections::HashMap::new()),
        cost_tracker: None,
        event_tx: tokio::sync::broadcast::channel(16).0,
        event_buffer: Arc::new(sse::EventBuffer::new(16)),
        shutdown_tx: tokio::sync::watch::channel(false).0,
        reload_tx: None,
        #[cfg(feature = "nodes")]
        node_registry: Arc::new(nodes::NodeRegistry::new(16)),
        #[cfg(feature = "nodes")]
        mdns_peer_registry: nodes::mdns::MdnsPeerRegistry::default(),
        path_prefix: String::new(),
        web_dist_dir: None,
        session_backend: None,
        session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
            8, 30, 600,
        )),
        device_registry: None,
        pending_pairings: None,
        cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ws_conversations: Default::default(),
        bridge_sockets: Default::default(),
    };

    let headers = HeaderMap::new();

    let body1 = Ok(Json(WebhookBody {
        message: "hello one".into(),
    }));
    let first = handle_webhook(
        State(state.clone()),
        test_connect_info(),
        Query(WebhookQuery::default()),
        headers.clone(),
        body1,
    )
    .await
    .into_response();
    assert_eq!(first.status(), StatusCode::OK);

    let body2 = Ok(Json(WebhookBody {
        message: "hello two".into(),
    }));
    let second = handle_webhook(
        State(state),
        test_connect_info(),
        Query(WebhookQuery::default()),
        headers,
        body2,
    )
    .await
    .into_response();
    assert_eq!(second.status(), StatusCode::OK);

    let keys = tracking_impl.keys.lock().clone();
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1]);
    assert!(keys[0].starts_with("webhook_msg_"));
    assert!(keys[1].starts_with("webhook_msg_"));
    assert_eq!(provider_impl.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn webhook_secret_hash_is_deterministic_and_nonempty() {
    let secret_a = generate_test_secret();
    let secret_b = generate_test_secret();
    let one = hash_webhook_secret(&secret_a);
    let two = hash_webhook_secret(&secret_a);
    let other = hash_webhook_secret(&secret_b);

    assert_eq!(one, two);
    assert_ne!(one, other);
    assert_eq!(one.len(), 64);
}

#[tokio::test]
async fn webhook_secret_hash_rejects_missing_header() {
    let provider_impl = Arc::new(MockModelProvider::default());
    let model_provider: Arc<dyn ModelProvider> = provider_impl.clone();
    let memory: Arc<dyn Memory> = Arc::new(MockMemory);
    let secret = generate_test_secret();

    let state = AppState {
        config: Arc::new(RwLock::new(Config::default())),
        config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
        model_provider,
        model: "test-model".into(),
        temperature: None,
        mem: memory.clone(),
        memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
            Arc::clone(&memory),
            zeroclaw_config::schema::MemoryConfig::default(),
            std::path::PathBuf::new(),
        )),
        companion_store: None,
        auto_save: false,
        webhook_secret_hash: Some(Arc::from(hash_webhook_secret(&secret))),
        pairing: Arc::new(PairingGuard::new(false, &[])),
        trust_forwarded_headers: false,
        rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
        auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
        idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
        observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
        tools_registry: Arc::new(Vec::new()),
        tools_registry_by_agent: Arc::new(std::collections::HashMap::new()),
        cost_tracker: None,
        event_tx: tokio::sync::broadcast::channel(16).0,
        event_buffer: Arc::new(sse::EventBuffer::new(16)),
        shutdown_tx: tokio::sync::watch::channel(false).0,
        reload_tx: None,
        #[cfg(feature = "nodes")]
        node_registry: Arc::new(nodes::NodeRegistry::new(16)),
        #[cfg(feature = "nodes")]
        mdns_peer_registry: nodes::mdns::MdnsPeerRegistry::default(),
        path_prefix: String::new(),
        web_dist_dir: None,
        session_backend: None,
        session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
            8, 30, 600,
        )),
        device_registry: None,
        pending_pairings: None,
        cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ws_conversations: Default::default(),
        bridge_sockets: Default::default(),
    };

    let response = handle_webhook(
        State(state),
        test_connect_info(),
        Query(WebhookQuery::default()),
        HeaderMap::new(),
        Ok(Json(WebhookBody {
            message: "hello".into(),
        })),
    )
    .await
    .into_response();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(provider_impl.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn webhook_secret_hash_rejects_invalid_header() {
    let provider_impl = Arc::new(MockModelProvider::default());
    let model_provider: Arc<dyn ModelProvider> = provider_impl.clone();
    let memory: Arc<dyn Memory> = Arc::new(MockMemory);
    let valid_secret = generate_test_secret();
    let wrong_secret = generate_test_secret();

    let state = AppState {
        config: Arc::new(RwLock::new(Config::default())),
        config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
        model_provider,
        model: "test-model".into(),
        temperature: None,
        mem: memory.clone(),
        memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
            Arc::clone(&memory),
            zeroclaw_config::schema::MemoryConfig::default(),
            std::path::PathBuf::new(),
        )),
        companion_store: None,
        auto_save: false,
        webhook_secret_hash: Some(Arc::from(hash_webhook_secret(&valid_secret))),
        pairing: Arc::new(PairingGuard::new(false, &[])),
        trust_forwarded_headers: false,
        rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
        auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
        idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
        observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
        tools_registry: Arc::new(Vec::new()),
        tools_registry_by_agent: Arc::new(std::collections::HashMap::new()),
        cost_tracker: None,
        event_tx: tokio::sync::broadcast::channel(16).0,
        event_buffer: Arc::new(sse::EventBuffer::new(16)),
        shutdown_tx: tokio::sync::watch::channel(false).0,
        reload_tx: None,
        #[cfg(feature = "nodes")]
        node_registry: Arc::new(nodes::NodeRegistry::new(16)),
        #[cfg(feature = "nodes")]
        mdns_peer_registry: nodes::mdns::MdnsPeerRegistry::default(),
        path_prefix: String::new(),
        web_dist_dir: None,
        session_backend: None,
        session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
            8, 30, 600,
        )),
        device_registry: None,
        pending_pairings: None,
        cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ws_conversations: Default::default(),
        bridge_sockets: Default::default(),
    };

    let mut headers = HeaderMap::new();
    headers.insert(
        "X-Webhook-Secret",
        HeaderValue::from_str(&wrong_secret).unwrap(),
    );

    let response = handle_webhook(
        State(state),
        test_connect_info(),
        Query(WebhookQuery::default()),
        headers,
        Ok(Json(WebhookBody {
            message: "hello".into(),
        })),
    )
    .await
    .into_response();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(provider_impl.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn webhook_secret_hash_accepts_valid_header() {
    let provider_impl = Arc::new(MockModelProvider::default());
    let model_provider: Arc<dyn ModelProvider> = provider_impl.clone();
    let memory: Arc<dyn Memory> = Arc::new(MockMemory);
    let secret = generate_test_secret();

    let state = AppState {
        config: Arc::new(RwLock::new(Config::default())),
        config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
        model_provider,
        model: "test-model".into(),
        temperature: None,
        mem: memory.clone(),
        memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
            Arc::clone(&memory),
            zeroclaw_config::schema::MemoryConfig::default(),
            std::path::PathBuf::new(),
        )),
        companion_store: None,
        auto_save: false,
        webhook_secret_hash: Some(Arc::from(hash_webhook_secret(&secret))),
        pairing: Arc::new(PairingGuard::new(false, &[])),
        trust_forwarded_headers: false,
        rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
        auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
        idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
        observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
        tools_registry: Arc::new(Vec::new()),
        tools_registry_by_agent: Arc::new(std::collections::HashMap::new()),
        cost_tracker: None,
        event_tx: tokio::sync::broadcast::channel(16).0,
        event_buffer: Arc::new(sse::EventBuffer::new(16)),
        shutdown_tx: tokio::sync::watch::channel(false).0,
        reload_tx: None,
        #[cfg(feature = "nodes")]
        node_registry: Arc::new(nodes::NodeRegistry::new(16)),
        #[cfg(feature = "nodes")]
        mdns_peer_registry: nodes::mdns::MdnsPeerRegistry::default(),
        path_prefix: String::new(),
        web_dist_dir: None,
        session_backend: None,
        session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
            8, 30, 600,
        )),
        device_registry: None,
        pending_pairings: None,
        cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ws_conversations: Default::default(),
        bridge_sockets: Default::default(),
    };

    let mut headers = HeaderMap::new();
    headers.insert("X-Webhook-Secret", HeaderValue::from_str(&secret).unwrap());

    let response = handle_webhook(
        State(state),
        test_connect_info(),
        Query(WebhookQuery::default()),
        headers,
        Ok(Json(WebhookBody {
            message: "hello".into(),
        })),
    )
    .await
    .into_response();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(provider_impl.calls.load(Ordering::SeqCst), 1);
}

// ══════════════════════════════════════════════════════════
// IdempotencyStore Edge-Case Tests
// ══════════════════════════════════════════════════════════

#[test]
fn idempotency_store_allows_different_keys() {
    let store = IdempotencyStore::new(Duration::from_secs(60), 100);
    assert!(store.record_if_new("key-a"));
    assert!(store.record_if_new("key-b"));
    assert!(store.record_if_new("key-c"));
    assert!(store.record_if_new("key-d"));
}

#[test]
fn idempotency_store_max_keys_clamped_to_one() {
    let store = IdempotencyStore::new(Duration::from_secs(60), 0);
    assert!(store.record_if_new("only-key"));
    assert!(!store.record_if_new("only-key"));
}

#[test]
fn idempotency_store_rapid_duplicate_rejected() {
    let store = IdempotencyStore::new(Duration::from_secs(300), 100);
    assert!(store.record_if_new("rapid"));
    assert!(!store.record_if_new("rapid"));
}

#[test]
fn idempotency_store_accepts_after_ttl_expires() {
    let store = IdempotencyStore::new(Duration::from_millis(1), 100);
    assert!(store.record_if_new("ttl-key"));
    std::thread::sleep(Duration::from_millis(10));
    assert!(store.record_if_new("ttl-key"));
}

#[test]
fn idempotency_store_eviction_preserves_newest() {
    let store = IdempotencyStore::new(Duration::from_secs(300), 1);
    assert!(store.record_if_new("old-key"));
    std::thread::sleep(Duration::from_millis(2));
    assert!(store.record_if_new("new-key"));

    let keys = store.keys.lock();
    assert_eq!(keys.len(), 1);
    assert!(!keys.contains_key("old-key"));
    assert!(keys.contains_key("new-key"));
}

#[test]
fn rate_limiter_allows_after_window_expires() {
    let window = Duration::from_millis(50);
    let limiter = SlidingWindowRateLimiter::new(2, window, 100);
    assert!(limiter.allow("ip-1"));
    assert!(limiter.allow("ip-1"));
    assert!(!limiter.allow("ip-1")); // blocked

    // Wait for window to expire
    std::thread::sleep(Duration::from_millis(60));

    // Should be allowed again
    assert!(limiter.allow("ip-1"));
}

#[test]
fn rate_limiter_independent_keys_tracked_separately() {
    let limiter = SlidingWindowRateLimiter::new(2, Duration::from_secs(60), 100);
    assert!(limiter.allow("ip-1"));
    assert!(limiter.allow("ip-1"));
    assert!(!limiter.allow("ip-1")); // ip-1 blocked

    // ip-2 should still work
    assert!(limiter.allow("ip-2"));
    assert!(limiter.allow("ip-2"));
    assert!(!limiter.allow("ip-2")); // ip-2 now blocked
}

#[test]
fn rate_limiter_exact_boundary_at_max_keys() {
    let limiter = SlidingWindowRateLimiter::new(10, Duration::from_secs(60), 3);
    assert!(limiter.allow("ip-1"));
    assert!(limiter.allow("ip-2"));
    assert!(limiter.allow("ip-3"));
    // At capacity now
    assert!(limiter.allow("ip-4")); // should evict ip-1

    let guard = limiter.requests.lock();
    assert_eq!(guard.0.len(), 3);
    assert!(
        !guard.0.contains_key("ip-1"),
        "ip-1 should have been evicted"
    );
    assert!(guard.0.contains_key("ip-2"));
    assert!(guard.0.contains_key("ip-3"));
    assert!(guard.0.contains_key("ip-4"));
}

#[test]
fn gateway_rate_limiter_pair_and_webhook_are_independent() {
    let limiter = GatewayRateLimiter::new(2, 3, 100);

    // Exhaust pair limit
    assert!(limiter.allow_pair("ip-1"));
    assert!(limiter.allow_pair("ip-1"));
    assert!(!limiter.allow_pair("ip-1")); // pair blocked

    // Webhook should still work
    assert!(limiter.allow_webhook("ip-1"));
    assert!(limiter.allow_webhook("ip-1"));
    assert!(limiter.allow_webhook("ip-1"));
    assert!(!limiter.allow_webhook("ip-1")); // webhook now blocked
}

#[test]
fn rate_limiter_single_key_max_allows_one_request() {
    let limiter = SlidingWindowRateLimiter::new(5, Duration::from_secs(60), 1);
    assert!(limiter.allow("ip-1"));
    assert!(limiter.allow("ip-2")); // evicts ip-1

    let guard = limiter.requests.lock();
    assert_eq!(guard.0.len(), 1);
    assert!(guard.0.contains_key("ip-2"));
    assert!(!guard.0.contains_key("ip-1"));
}

#[test]
fn rate_limiter_concurrent_access_safe() {
    use std::sync::Arc;

    let limiter = Arc::new(SlidingWindowRateLimiter::new(
        1000,
        Duration::from_secs(60),
        1000,
    ));
    let mut handles = Vec::new();

    for i in 0..10 {
        let limiter = limiter.clone();
        handles.push(std::thread::spawn(move || {
            for j in 0..100 {
                limiter.allow(&format!("thread-{i}-req-{j}"));
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // Should not panic or deadlock
    let guard = limiter.requests.lock();
    assert!(guard.0.len() <= 1000, "should respect max_keys");
}

#[test]
fn idempotency_store_concurrent_access_safe() {
    use std::sync::Arc;

    let store = Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000));
    let mut handles = Vec::new();

    for i in 0..10 {
        let store = store.clone();
        handles.push(std::thread::spawn(move || {
            for j in 0..100 {
                store.record_if_new(&format!("thread-{i}-key-{j}"));
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let keys = store.keys.lock();
    assert!(keys.len() <= 1000, "should respect max_keys");
}

#[test]
fn rate_limiter_rapid_burst_then_cooldown() {
    let limiter = SlidingWindowRateLimiter::new(5, Duration::from_millis(50), 100);

    // Burst: use all 5 requests
    for _ in 0..5 {
        assert!(limiter.allow("burst-ip"));
    }
    assert!(!limiter.allow("burst-ip")); // 6th should fail

    // Cooldown
    std::thread::sleep(Duration::from_millis(60));

    // Should be allowed again
    assert!(limiter.allow("burst-ip"));
}

#[test]
fn require_localhost_accepts_ipv4_loopback() {
    let peer = SocketAddr::from(([127, 0, 0, 1], 12345));
    assert!(require_localhost(&peer).is_ok());
}

#[test]
fn require_localhost_accepts_ipv6_loopback() {
    let peer = SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 12345));
    assert!(require_localhost(&peer).is_ok());
}

#[test]
fn require_localhost_rejects_non_loopback_ipv4() {
    let peer = SocketAddr::from(([192, 168, 1, 100], 12345));
    let err = require_localhost(&peer).unwrap_err();
    assert_eq!(err.0, StatusCode::FORBIDDEN);
}

#[test]
fn require_localhost_rejects_non_loopback_ipv6() {
    let peer = SocketAddr::from((
        std::net::Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1),
        12345,
    ));
    let err = require_localhost(&peer).unwrap_err();
    assert_eq!(err.0, StatusCode::FORBIDDEN);
}

#[test]
fn admin_reload_gate_loopback_always_allowed() {
    // Loopback is allowed regardless of the opt-in or pairing flags.
    assert_eq!(
        admin_reload_gate(true, false, false),
        AdminReloadGate::Allow
    );
    assert_eq!(admin_reload_gate(true, true, true), AdminReloadGate::Allow);
    assert_eq!(admin_reload_gate(true, false, true), AdminReloadGate::Allow);
    assert_eq!(admin_reload_gate(true, true, false), AdminReloadGate::Allow);
}

#[test]
fn admin_reload_gate_remote_blocked_by_default() {
    // Non-loopback caller with the flag off is rejected outright,
    // regardless of pairing.
    assert_eq!(
        admin_reload_gate(false, false, true),
        AdminReloadGate::Forbidden
    );
    assert_eq!(
        admin_reload_gate(false, false, false),
        AdminReloadGate::Forbidden
    );
}

#[test]
fn admin_reload_gate_remote_opt_in_requires_auth() {
    // Non-loopback caller with the flag on and pairing on must authenticate.
    assert_eq!(
        admin_reload_gate(false, true, true),
        AdminReloadGate::RequireAuth
    );
}

#[test]
fn admin_reload_gate_remote_opt_in_without_pairing_is_rejected() {
    // Opting in with pairing off cannot authenticate the caller, so the
    // request is rejected rather than allowed anonymously.
    assert_eq!(
        admin_reload_gate(false, true, false),
        AdminReloadGate::ForbiddenNoPairing
    );
}

#[test]
fn allow_remote_admin_defaults_off() {
    // Security default: remote admin reload is disabled until opted in.
    assert!(!zeroclaw_config::schema::GatewayConfig::default().allow_remote_admin);
}

/// Build an `AppState` for `handle_admin_reload`: controls
/// `gateway.allow_remote_admin`, pairing (and its tokens), and wires a
/// live reload channel so the allowed path reaches `200` rather than the
/// `503` standalone-gateway branch.
fn admin_reload_state(
    tmp: &tempfile::TempDir,
    allow_remote_admin: bool,
    require_pairing: bool,
    tokens: &[String],
) -> AppState {
    let mut state = admin_paircode_state(tmp, require_pairing, false);
    state.config.write().gateway.allow_remote_admin = allow_remote_admin;
    state.pairing = Arc::new(PairingGuard::new(require_pairing, tokens));
    state.reload_tx = Some(tokio::sync::watch::channel(false).0);
    state
}

fn loopback_peer() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 40000))
}

fn remote_peer() -> SocketAddr {
    // RFC 5737 TEST-NET-3 documentation address — a stable non-loopback
    // peer that is never a real host on anyone's network.
    SocketAddr::from(([203, 0, 113, 50], 40000))
}

fn bearer_headers(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    );
    headers
}

#[tokio::test]
async fn admin_reload_loopback_no_token_reloads() {
    let tmp = tempfile::tempdir().unwrap();
    let state = admin_reload_state(&tmp, false, true, &[]);
    let resp = handle_admin_reload(State(state), ConnectInfo(loopback_peer()), HeaderMap::new())
        .await
        .unwrap()
        .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn admin_reload_remote_default_off_is_forbidden() {
    let tmp = tempfile::tempdir().unwrap();
    let state = admin_reload_state(&tmp, false, true, &[]);
    let err = handle_admin_reload(State(state), ConnectInfo(remote_peer()), HeaderMap::new())
        .await
        .err()
        .unwrap();
    assert_eq!(err.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_reload_remote_opt_in_without_pairing_does_not_reload() {
    // The fixed hole: allow_remote_admin = true + require_pairing = false
    // must NOT permit an anonymous remote reload.
    let tmp = tempfile::tempdir().unwrap();
    let state = admin_reload_state(&tmp, true, false, &[]);
    let err = handle_admin_reload(State(state), ConnectInfo(remote_peer()), HeaderMap::new())
        .await
        .err()
        .unwrap();
    assert_eq!(err.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_reload_remote_opt_in_missing_token_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let state = admin_reload_state(&tmp, true, true, &["zc_test_token".to_string()]);
    let err = handle_admin_reload(State(state), ConnectInfo(remote_peer()), HeaderMap::new())
        .await
        .err()
        .unwrap();
    assert_eq!(err.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_reload_remote_opt_in_invalid_token_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let state = admin_reload_state(&tmp, true, true, &["zc_test_token".to_string()]);
    let err = handle_admin_reload(
        State(state),
        ConnectInfo(remote_peer()),
        bearer_headers("not-the-token"),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(err.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_reload_remote_opt_in_valid_token_reloads() {
    let tmp = tempfile::tempdir().unwrap();
    let state = admin_reload_state(&tmp, true, true, &["zc_test_token".to_string()]);
    let resp = handle_admin_reload(
        State(state),
        ConnectInfo(remote_peer()),
        bearer_headers("zc_test_token"),
    )
    .await
    .unwrap()
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[test]
fn needs_quickstart_for_flags_empty_model() {
    let err = needs_quickstart_for("").expect("empty model must produce a needs_quickstart error");
    let msg = err.to_string();
    assert!(
        msg.contains("needs_quickstart"),
        "error must carry the needs_quickstart marker for callers to map to 503; got: {msg}"
    );
    assert!(
        msg.contains("zeroclaw quickstart"),
        "error must point the user at `zeroclaw quickstart`; got: {msg}"
    );
}

#[test]
fn needs_quickstart_for_flags_whitespace_only_model() {
    assert!(
        needs_quickstart_for("   ").is_some(),
        "whitespace-only model must be treated as empty"
    );
    assert!(
        needs_quickstart_for("\n\t ").is_some(),
        "tabs and newlines count as empty too"
    );
}

#[test]
fn needs_quickstart_for_passes_real_model() {
    assert!(
        needs_quickstart_for("anthropic/claude-sonnet-4").is_none(),
        "a real model id must not be flagged"
    );
    assert!(
        needs_quickstart_for("  gpt-4  ").is_none(),
        "leading/trailing whitespace around a real model id must not be flagged"
    );
}

#[test]
fn is_needs_quickstart_err_detects_marker_from_helper() {
    let err = needs_quickstart_for("").expect("empty model produces marker");
    assert!(
        is_needs_quickstart_err(&err),
        "the marker emitted by needs_quickstart_for must be detected"
    );
}

#[test]
fn is_needs_quickstart_err_ignores_unrelated_errors() {
    let err = anyhow::Error::msg("upstream timeout: provider returned 504");
    assert!(
        !is_needs_quickstart_err(&err),
        "unrelated errors must not be misclassified as needs_quickstart"
    );
    let err = anyhow::Error::msg("invalid api key");
    assert!(!is_needs_quickstart_err(&err));
}

#[test]
fn is_needs_quickstart_err_detects_via_substring() {
    // Defends the contract that the substring marker is the
    // detection key — not the exact string. Wrappers (e.g.
    // anyhow::Error::context) must not break the check.
    let err = anyhow::Error::msg("provider call failed").context("needs_quickstart: empty model");
    assert!(is_needs_quickstart_err(&err));
}

#[test]
fn needs_quickstart_channel_reply_resolves_via_fluent() {
    let reply = needs_quickstart_channel_reply();
    assert!(
        !reply.starts_with('{') && !reply.ends_with('}'),
        "fluent missing-key fallback leaked into channel reply: {reply:?}"
    );
    assert!(
        reply.to_lowercase().contains("quickstart"),
        "channel reply must mention Quickstart so users know what's missing: {reply:?}"
    );
}

/// Build an `AppState` whose device registry points at a non-existent
/// path so every SQLite write fails. Mirrors `unwriteable_registry_state`
/// in `api_pairing::tests` so the regression set stays side-by-side.
fn unwriteable_registry_pair_state(tmp: &tempfile::TempDir) -> AppState {
    let mut state = admin_paircode_state(tmp, true, false);
    // No registry from `admin_paircode_state`; inject the broken one.
    state.device_registry = Some(Arc::new(api_pairing::DeviceRegistry::with_db_path(
        std::path::PathBuf::from("/this/path/does/not/exist/devices.db"),
    )));
    state
}

async fn legacy_pair_response_json(result: impl IntoResponse) -> (StatusCode, serde_json::Value) {
    let response = result.into_response();
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("legacy /pair response body")
        .to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    (status, body)
}

#[tokio::test]
async fn legacy_pair_rolls_back_in_process_token_when_registry_register_fails() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = unwriteable_registry_pair_state(&tmp);

    let code = state
        .pairing
        .generate_new_pairing_code()
        .expect("pairing code must be issuable when require_pairing=true");

    let mut headers = HeaderMap::new();
    headers.insert("X-Pairing-Code", HeaderValue::from_str(&code).unwrap());

    let (status, body) = legacy_pair_response_json(
        handle_pair(State(state.clone()), test_connect_info(), headers).await,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "legacy /pair registry.register failure must surface as 500"
    );
    assert_eq!(body["paired"], serde_json::Value::Bool(false));
    assert!(
        body.get("token").is_none(),
        "legacy /pair 5xx body MUST NOT contain the plaintext bearer token; got: {body}"
    );
    assert!(
        state.pairing.tokens().is_empty(),
        "PairingGuard::paired_tokens must be empty after a failed /pair \
         registry.register (compensating `revoke_token_hash`); instead have {:?}",
        state.pairing.tokens()
    );
}

#[tokio::test]
async fn legacy_pair_rolls_back_in_process_token_when_persist_fails() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = admin_paircode_state(&tmp, true, false);
    let blocker = tmp.path().join("legacy-pair-blocker");
    std::fs::write(&blocker, b"").expect("seed blocker file");
    state.config.write().config_path = blocker.join("config.toml");

    let code = state
        .pairing
        .generate_new_pairing_code()
        .expect("pairing code must be issuable when require_pairing=true");

    let mut headers = HeaderMap::new();
    headers.insert("X-Pairing-Code", HeaderValue::from_str(&code).unwrap());

    let (status, body) = legacy_pair_response_json(
        handle_pair(State(state.clone()), test_connect_info(), headers).await,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "legacy /pair persistence failure MUST surface as 500 (legacy leaked 200 + token)"
    );
    assert_eq!(body["paired"], serde_json::Value::Bool(false));
    assert!(
        body.get("token").is_none(),
        "legacy /pair 5xx body MUST NOT contain the plaintext bearer token; got: {body}"
    );
    assert!(
        state.pairing.tokens().is_empty(),
        "PairingGuard::paired_tokens must be empty after a failed /pair \
         persist; have {:?}",
        state.pairing.tokens()
    );
}

#[test]
fn public_bind_without_pairing_needs_an_explicit_opt_in() {
    use PublicBindPosture::*;
    // Loopback is always fine, pairing or not.
    assert_eq!(
        public_bind_posture("127.0.0.1", false, false, true, false),
        Local
    );
    assert_eq!(
        public_bind_posture("::1", false, false, false, false),
        Local
    );
    for host in ["127.0.0.2", "[0:0:0:0:0:0:0:1]"] {
        assert_eq!(public_bind_posture(host, false, true, true, false), Local);
    }
    for host in ["0.0.0.0", "[::]", "192.168.1.10"] {
        for allow_public_bind in [false, true] {
            for no_tunnel in [false, true] {
                assert_eq!(
                    public_bind_posture(host, false, allow_public_bind, no_tunnel, false),
                    RefuseUnauthenticated
                );
                assert_eq!(
                    public_bind_posture(host, false, allow_public_bind, no_tunnel, true),
                    UnauthenticatedOptedIn
                );
            }
        }
    }
    // Network-facing without pairing: every client is anonymous, so any of
    // them could attach to any shared session. Refused unless opted in; a
    // tunnel is not an opt-in.
    assert_eq!(
        public_bind_posture("0.0.0.0", false, false, true, false),
        RefuseUnauthenticated
    );
    assert_eq!(
        public_bind_posture("192.168.1.10", false, false, false, false),
        RefuseUnauthenticated
    );
    assert_eq!(
        public_bind_posture("0.0.0.0", false, true, true, false),
        RefuseUnauthenticated
    );
    // With pairing, the pre-existing warning is unchanged.
    assert_eq!(
        public_bind_posture("0.0.0.0", true, false, true, false),
        ExposedWithoutOptIn
    );
    assert_eq!(
        public_bind_posture("0.0.0.0", true, false, false, false),
        Local
    );
    assert_eq!(
        public_bind_posture("0.0.0.0", true, true, true, false),
        Local
    );
}

/// Send a WebSocket upgrade for `/ws/chat` to a live server and return the
/// HTTP status code of the response.
async fn ws_chat_upgrade_status(addr: SocketAddr, query: &str, bearer: Option<&str>) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let auth = bearer
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "GET /ws/chat?{query} HTTP/1.1\r\nHost: {addr}\r\nConnection: Upgrade\r\n\
         Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n{auth}\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut buf = vec![0u8; 256];
    let n = stream.read(&mut buf).await.unwrap();
    let head = String::from_utf8_lossy(&buf[..n]);
    head.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no HTTP status in {head:?}"))
}

/// Trust model for shared sessions (#397): with pairing on, attaching to a
/// session is gated by the paired token, not by the session id. A client
/// that names someone's session id without a valid token is refused before
/// the upgrade; any paired token (every paired device is the owner's) gets
/// past the auth gate for any session id.
#[tokio::test]
async fn ws_chat_attach_to_an_existing_session_requires_a_paired_token() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = admin_paircode_state(&tmp, true, true);
    let token = pair_device(&state, "owner-phone").await;
    let app = Router::new()
        .route("/ws/chat", get(ws::handle_ws_chat))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let client = async {
        let victim = "agent=ghost&session_id=owners-session";
        assert_eq!(ws_chat_upgrade_status(addr, victim, None).await, 401);
        assert_eq!(
            ws_chat_upgrade_status(addr, victim, Some("not-a-paired-token")).await,
            401
        );
        assert_eq!(
            ws_chat_upgrade_status(addr, &format!("{victim}&token=zc_forged"), None).await,
            401
        );
        // The paired token passes auth; the request then fails on the
        // unknown agent (400), proving the 401s above came from the gate.
        assert_eq!(
            ws_chat_upgrade_status(addr, victim, Some(&token)).await,
            400
        );
    };
    tokio::select! {
        result = axum::serve(listener, app.into_make_service()) => {
            panic!("server exited early: {result:?}")
        }
        () = client => {}
    }
}

// Owner-token contract: exercise the HTTP handlers through Axum, including
// loopback peers (which can also represent a same-host reverse proxy).
async fn owner_paircode_request(
    state: AppState,
    method: &str,
    path: &str,
    peer: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, serde_json::Value) {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let router = Router::new()
        .route("/pair/code", get(handle_pair_code))
        .route("/admin/paircode", get(handle_admin_paircode))
        .route("/admin/paircode/new", post(handle_admin_paircode_new))
        .route("/pair", post(handle_pair))
        .with_state(state);
    let mut request = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let mut request = request.body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn owner_paircode_public_never_discloses_pending_code() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = admin_paircode_state(&tmp, true, false);
    assert!(state.pairing.pairing_code().is_some());
    for peer in ["127.0.0.1:40000", "[::1]:40000", "203.0.113.7:40000"] {
        for headers in [vec![], vec![("x-forwarded-for", "203.0.113.7")]] {
            let (status, json) =
                owner_paircode_request(state.clone(), "GET", "/pair/code", peer, &headers).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(json["pairing_required"], true);
            assert!(
                json["pairing_code"].is_null(),
                "public response must not contain a pairing code"
            );
        }
    }
}

#[tokio::test]
async fn owner_paircode_admin_requires_token_even_on_loopback() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = admin_paircode_state(&tmp, true, false);
    let before = state.pairing.pairing_code();
    for (method, path) in [("GET", "/admin/paircode"), ("POST", "/admin/paircode/new")] {
        for headers in [
            vec![],
            vec![("x-forwarded-for", "203.0.113.7")],
            vec![("x-zeroclaw-admin-token", "")],
            vec![("x-zeroclaw-admin-token", "invalid")],
        ] {
            let (status, json) =
                owner_paircode_request(state.clone(), method, path, "127.0.0.1:40000", &headers)
                    .await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert!(json.get("pairing_code").is_none());
        }
    }
    assert_eq!(state.pairing.pairing_code(), before);
}

#[tokio::test]
async fn owner_paircode_manual_first_pairing_remains_available() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = admin_paircode_state(&tmp, true, true);
    // The startup banner reads this same code; the browser submits it manually.
    let code = state.pairing.pairing_code().unwrap();
    let (status, json) = owner_paircode_request(
        state.clone(),
        "POST",
        "/pair",
        "127.0.0.1:40000",
        &[("x-pairing-code", &code)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["paired"], true);
    let token = json["token"].as_str().unwrap();
    assert!(state.pairing.is_authenticated(token));
    assert!(
        state
            .config
            .read()
            .gateway
            .paired_tokens
            .contains(&PairingGuard::token_hash(token))
    );
    assert!(state.pairing.pairing_code().is_none());
}

/// Headers carrying this test gateway's admin secret, minted the way a
/// gateway start mints it.
fn admin_headers(state: &AppState) -> HeaderMap {
    let data_dir = state.config.read().data_dir.clone();
    let secret = state
        .pairing
        .rotate_admin_token(&data_dir)
        .expect("rotate admin token");
    let mut headers = HeaderMap::new();
    headers.insert(
        GATEWAY_ADMIN_TOKEN_HEADER,
        HeaderValue::from_str(&secret).expect("admin token is a valid header value"),
    );
    headers
}

/// Headers a same-host reverse proxy or tunnel produces for a remote
/// caller. Some proxies add a forwarding header and some add none; the
/// admin gates must not depend on either.
fn proxied_headers(with_forwarding_header: bool) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if with_forwarding_header {
        headers.insert("X-Forwarded-For", HeaderValue::from_static("203.0.113.7"));
    }
    headers
}

async fn json_of(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn pair_with(state: &AppState, code: &str) -> (StatusCode, serde_json::Value) {
    let mut headers = HeaderMap::new();
    headers.insert(
        "X-Pairing-Code",
        HeaderValue::from_str(code).expect("code is a valid header value"),
    );
    json_of(
        handle_pair(State(state.clone()), test_connect_info(), headers)
            .await
            .into_response(),
    )
    .await
}

#[tokio::test]
async fn pair_code_never_returns_the_code_even_to_a_loopback_caller() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = admin_paircode_state(&tmp, true, false);
    assert!(
        state.pairing.pairing_code().is_some(),
        "a fresh guard holds a first-run code"
    );

    let (status, json) = json_of(handle_pair_code(State(state)).await.into_response()).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["pairing_required"], true);
    assert!(
        json["pairing_code"].is_null(),
        "no HTTP caller can prove it is local, so the code is never served: {json}"
    );
}

#[tokio::test]
async fn admin_paircode_refuses_loopback_callers_without_the_admin_token() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = admin_paircode_state(&tmp, true, false);
    let _current = admin_headers(&state);

    let mut wrong = HeaderMap::new();
    wrong.insert(
        GATEWAY_ADMIN_TOKEN_HEADER,
        HeaderValue::from_static("zc_wrong"),
    );
    for headers in [proxied_headers(true), proxied_headers(false), wrong] {
        let (status, json) = admin_paircode_response_json(
            handle_admin_paircode(State(state.clone()), test_connect_info(), headers).await,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{json}");
        assert!(json.get("pairing_code").is_none(), "{json}");
    }
}

#[tokio::test]
async fn admin_paircode_serves_the_code_with_the_admin_token() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = admin_paircode_state(&tmp, true, false);
    let expected = state.pairing.pairing_code();

    let (status, json) = admin_paircode_response_json(
        handle_admin_paircode(
            State(state.clone()),
            test_connect_info(),
            admin_headers(&state),
        )
        .await,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["pairing_code"].as_str(), expected.as_deref());
}

#[tokio::test]
async fn admin_paircode_new_refuses_loopback_callers_without_the_admin_token() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = admin_paircode_state(&tmp, true, true);
    let _current = admin_headers(&state);
    let before = state.pairing.pairing_code();

    for headers in [proxied_headers(true), proxied_headers(false)] {
        let (status, _json) = admin_paircode_response_json(
            handle_admin_paircode_new(
                State(state.clone()),
                test_connect_info(),
                headers,
                Query(AdminPaircodeQuery::default()),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    assert_eq!(
        state.pairing.pairing_code(),
        before,
        "a refused mint must not issue or replace a code"
    );
}

#[tokio::test]
async fn admin_token_from_an_earlier_gateway_start_is_refused() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = admin_paircode_state(&tmp, true, false);
    let stale = admin_headers(&state);
    let _restart = admin_headers(&state);

    let (status, _json) = admin_paircode_response_json(
        handle_admin_paircode(State(state), test_connect_info(), stale).await,
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// A rotation that cannot write its file must fail closed: the token that
/// worked before, still sitting in the file, is refused afterwards.
#[tokio::test]
async fn failed_admin_token_rotation_refuses_the_token_left_on_disk() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = admin_paircode_state(&tmp, true, false);
    let working = admin_headers(&state);
    let data_dir = state.config.read().data_dir.clone();
    let on_disk = std::fs::read_to_string(gateway_admin_token_path(&data_dir)).unwrap();

    let unwritable = tmp.path().join("not-a-dir");
    std::fs::write(&unwritable, b"x").unwrap();
    assert!(state.pairing.rotate_admin_token(&unwritable).is_err());
    assert_eq!(
        std::fs::read_to_string(gateway_admin_token_path(&data_dir)).unwrap(),
        on_disk,
        "the previous token file is still in place"
    );

    let (status, json) = admin_paircode_response_json(
        handle_admin_paircode(State(state), test_connect_info(), working).await,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{json}");
    assert!(json.get("pairing_code").is_none(), "{json}");
}

/// The composed attack from review A1: a remote caller relayed from
/// loopback by a same-host proxy tries every route that reads or mints a
/// code, then tries to pair. It must end with no code, no paired token and
/// no authenticated access. A caller holding the admin token completes the
/// same sequence, so the admin token is the only thing standing between.
#[tokio::test]
async fn proxied_loopback_caller_cannot_reach_operator_access_through_any_code_route() {
    for with_forwarding_header in [true, false] {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = admin_paircode_state(&tmp, true, true);
        let _current = admin_headers(&state);

        let (_, public) =
            json_of(handle_pair_code(State(state.clone())).await.into_response()).await;
        assert!(public["pairing_code"].is_null());

        let (read_status, _) = admin_paircode_response_json(
            handle_admin_paircode(
                State(state.clone()),
                test_connect_info(),
                proxied_headers(with_forwarding_header),
            )
            .await,
        )
        .await;
        assert_eq!(read_status, StatusCode::FORBIDDEN);

        let (mint_status, _) = admin_paircode_response_json(
            handle_admin_paircode_new(
                State(state.clone()),
                test_connect_info(),
                proxied_headers(with_forwarding_header),
                Query(AdminPaircodeQuery::default()),
            )
            .await,
        )
        .await;
        assert_eq!(mint_status, StatusCode::FORBIDDEN);

        // With no code in hand, a guess cannot pair.
        let (pair_status, _) = pair_with(&state, "not-a-six-digit-code").await;
        assert_ne!(pair_status, StatusCode::OK);
        assert!(!state.pairing.is_paired(), "no token may have been minted");
        assert!(
            api::require_auth(&state, &HeaderMap::new()).is_err(),
            "the caller must end with no authenticated access"
        );
    }

    // Control: the same sequence with the admin token reaches a bearer.
    let tmp = tempfile::TempDir::new().unwrap();
    let state = admin_paircode_state(&tmp, true, true);
    let (_, minted) = admin_paircode_response_json(
        handle_admin_paircode_new(
            State(state.clone()),
            test_connect_info(),
            admin_headers(&state),
            Query(AdminPaircodeQuery::default()),
        )
        .await,
    )
    .await;
    let code = minted["pairing_code"]
        .as_str()
        .expect("admin mint issues a code");
    let (pair_status, paired) = pair_with(&state, code).await;
    assert_eq!(pair_status, StatusCode::OK, "{paired}");
    let bearer = paired["token"].as_str().expect("pairing returns a bearer");
    let mut auth = HeaderMap::new();
    auth.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {bearer}")).unwrap(),
    );
    assert!(api::require_auth(&state, &auth).is_ok());
}

#[tokio::test]
async fn container_public_bind_without_pairing_refuses_startup() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut config = Config {
        data_dir: tmp.path().join("workspace"),
        config_path: tmp.path().join("config.toml"),
        ..Config::default()
    };
    config.gateway.allow_public_bind = true;
    config.gateway.require_pairing = false;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        run_gateway("0.0.0.0", 0, config, None, None, None),
    )
    .await
    .expect("unsafe gateway must refuse startup instead of serving");
    let error = result.expect_err("container network opt-in must not disable authentication");
    assert!(
        error
            .to_string()
            .contains("dangerously_allow_unauthenticated_public_bind"),
        "{error}"
    );
}

#[tokio::test]
async fn container_bind_startup_allowed_matrix_serves_health() {
    for (host, dangerous_exemption, require_pairing) in [
        ("0.0.0.0", true, false),
        ("127.0.0.1", false, false),
        ("[0:0:0:0:0:0:0:1]", false, false),
        ("0.0.0.0", false, true),
    ] {
        let tmp = tempfile::TempDir::new().unwrap();
        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let mut config = Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.gateway.allow_public_bind = true;
        config.gateway.require_pairing = require_pairing;
        config.gateway.dangerously_allow_unauthenticated_public_bind = dangerous_exemption;
        let (shutdown_tx, _) = tokio::sync::watch::channel(false);
        let (reload_tx, _) = tokio::sync::watch::channel(false);
        let controls = zeroclaw_runtime::daemon::GatewayReloadControls {
            shutdown_tx: shutdown_tx.clone(),
            reload_tx,
        };
        let handle = zeroclaw_spawn::spawn!(async move {
            run_gateway(host, port, config, None, Some(controls), None).await
        });
        let response = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                if let Ok(mut stream) = tokio::net::TcpStream::connect(
                    zeroclaw_infra::parse_gateway_bind_socket_addr(
                        if host == "0.0.0.0" { "127.0.0.1" } else { host },
                        port,
                    )
                    .unwrap(),
                )
                .await
                {
                    stream
                        .write_all(
                            b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                        )
                        .await
                        .unwrap();
                    let mut response = String::new();
                    stream.read_to_string(&mut response).await.unwrap();
                    break response;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await;
        if response.is_err() {
            handle.abort();
        }
        assert!(
            response
                .expect("allowed gateway must serve HTTP")
                .starts_with("HTTP/1.1 200")
        );
        shutdown_tx.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

/// Exercise production startup and the HTTP state projection, not a simulated
/// store opener: restart must settle old work, while a competing port must not.
#[tokio::test]
async fn gateway_restart_settles_abandoned_turns_and_refuses_a_second_owner() {
    use zeroclaw_infra::session_backend::SessionBackend;
    let tmp = tempfile::tempdir().unwrap();
    let mut config = Config {
        data_dir: tmp.path().join("data"),
        config_path: tmp.path().join("config.toml"),
        ..Config::default()
    };
    config.gateway.require_pairing = false;
    config.gateway.session_persistence = true;
    let backend =
        zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(&config.data_dir).unwrap();
    backend
        .append(
            "gw_restart",
            &zeroclaw_api::model_provider::ChatMessage::user("preserved input"),
        )
        .unwrap();
    backend
        .set_session_state("gw_restart", "running", Some("crashed-turn"))
        .unwrap();
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let (shutdown_tx, _) = tokio::sync::watch::channel(false);
    let (reload_tx, _) = tokio::sync::watch::channel(false);
    let controls = zeroclaw_runtime::daemon::GatewayReloadControls {
        shutdown_tx: shutdown_tx.clone(),
        reload_tx,
    };
    let first_config = config.clone();
    let task = zeroclaw_spawn::spawn!(async move {
        run_gateway("127.0.0.1", port, first_config, None, Some(controls), None).await
    });
    let response = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok(mut stream) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                stream.write_all(b"GET /api/sessions/restart/state HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").await.unwrap();
                let mut response = String::new();
                stream.read_to_string(&mut response).await.unwrap();
                break response;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }).await.expect("gateway must serve restored session state");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let body: serde_json::Value =
        serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["state"], "error");
    assert_eq!(body["turn_id"], "crashed-turn");
    assert_eq!(backend.load("gw_restart")[0].content, "preserved input");
    // A new turn is live now: a second gateway, even on a different port,
    // must fail before resetting it or admitting more work.
    backend
        .set_session_state("gw_restart", "running", Some("live-turn"))
        .unwrap();
    let refused = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        run_gateway("127.0.0.1", 0, config, None, None, None),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(
        refused.to_string().contains("another gateway still owns"),
        "{refused}"
    );
    assert_eq!(
        backend
            .get_session_state("gw_restart")
            .unwrap()
            .unwrap()
            .state,
        "running"
    );
    shutdown_tx.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn gateway_shutdown_observes_a_signal_sent_before_subscription() {
    let (signal, _receiver) = tokio::sync::watch::channel(false);
    signal.send(true).unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        gateway_shutdown(signal.subscribe()),
    )
    .await
    .unwrap();
}
