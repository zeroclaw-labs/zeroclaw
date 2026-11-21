//! The real gateway serving connections a relay-served dashboard forwarded.
//!
//! Each test starts `run_gateway_with_plugin_webhooks`, with every route
//! mounted, on a loopback port, attached to a [`DashboardIngress`] the test
//! owns. It then hands the gateway in-memory connections exactly as the
//! relay bridge does, and compares what they get with what a socket on this
//! machine gets. The property under test is that a relay-forwarded browser is
//! never this machine: not to the admin routes, not to the limiters, and not
//! by claiming a loopback address in a forwarded header. Nor does it pair
//! or present a paired token: the relay reads everything on these
//! connections, so only a dashboard sign-in authenticates on them.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zeroclaw_config::schema::Config;
use zeroclaw_runtime::relay_dashboard::{
    DashboardConn, DashboardIngress, DashboardOrigin, DeliverError,
};

const STEP: Duration = Duration::from_secs(20);
const DASHBOARD_HOST: &str = "abc.relay.example";
/// The browser, as the relay reports it.
const BROWSER: &str = "203.0.113.9:51000";

struct Gateway {
    addr: std::net::SocketAddr,
    data_dir: std::path::PathBuf,
    ingress: DashboardIngress,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    server: tokio::task::JoinHandle<anyhow::Result<()>>,
    _root: tempfile::TempDir,
}

fn config(root: &Path) -> Config {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let mut config = Config {
        data_dir: workspace,
        config_path: root.join("config.toml"),
        ..Config::default()
    };
    config.gateway.require_pairing = true;
    // A proxy in front of the listener is trusted, which is exactly when a
    // browser-written forwarded header could otherwise claim an address.
    config.gateway.trust_forwarded_headers = true;
    // No per-minute cap on `/pair`, so the attempt lockout is what answers.
    config.gateway.pair_rate_limit_per_minute = 0;
    config.memory.backend = "none".to_string();
    config.memory.auto_save = false;
    config
}

impl Gateway {
    async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    async fn start_with(change: impl FnOnce(&mut Config)) -> Self {
        let root = tempfile::TempDir::new().expect("temp root");
        let mut config = config(root.path());
        change(&mut config);
        let data_dir = config.data_dir.clone();
        let authority = zeroclaw_runtime::LiveConfigAuthority::new_owned(config.clone())
            .expect("live config authority");
        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("probe port");
        let port = probe.local_addr().expect("probe address").port();
        drop(probe);

        let (shutdown_tx, _) = tokio::sync::watch::channel(false);
        let (reload_tx, _) = tokio::sync::watch::channel(false);
        let reload_controls = zeroclaw_runtime::daemon::GatewayReloadControls::standalone(
            shutdown_tx.clone(),
            reload_tx,
        );
        let (ready_tx, mut ready_rx) = tokio::sync::watch::channel(None);
        let readiness = zeroclaw_runtime::daemon::GatewayReadinessReporter::new(move |addr| {
            let _ = ready_tx.send(Some(addr));
        });
        let ingress = DashboardIngress::new();
        let supervision = zeroclaw_gateway::GatewaySupervision::new(
            Some(readiness),
            Arc::new(zeroclaw_api::webhook::PluginWebhookRegistry::new()),
            authority,
            None,
        )
        .with_dashboard_ingress(ingress.clone());
        let server = zeroclaw_spawn::spawn!(Box::pin(
            zeroclaw_gateway::run_gateway_with_plugin_webhooks(
                "127.0.0.1",
                port,
                config,
                None,
                Some(reload_controls),
                None,
                None,
                None,
                None,
                None,
                supervision,
            )
        ));
        let addr = tokio::time::timeout(STEP, async {
            ready_rx.wait_for(Option::is_some).await.expect("readiness");
            ready_rx.borrow().expect("bound address")
        })
        .await
        .expect("the gateway reports its bind");
        tokio::time::timeout(STEP, async {
            while !ingress.is_attached() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the gateway attaches to the dashboard ingress");
        Self {
            addr,
            data_dir,
            ingress,
            shutdown_tx,
            server,
            _root: root,
        }
    }

    /// One request on a connection the relay bridge hands over for the
    /// browser at `browser`.
    async fn via_relay(&self, browser: &str, request: &str) -> Reply {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let origin = DashboardOrigin::new(DASHBOARD_HOST, 443).expect("origin");
        let conn = DashboardConn::new(theirs, browser.parse().expect("address"), origin)
            .expect("a public browser address");
        self.ingress
            .deliver(conn)
            .unwrap_or_else(|error| panic!("delivered: {}", error.as_str()));
        exchange(ours, request).await
    }

    /// The same request on a socket from this machine.
    async fn direct(&self, request: &str) -> Reply {
        let socket = tokio::net::TcpStream::connect(self.addr)
            .await
            .expect("connect");
        exchange(socket, request).await
    }

    /// A fresh pairing code, minted from this machine with the admin token
    /// the gateway wrote for its run.
    async fn mint_pairing_code(&self) -> String {
        let admin_token = std::fs::read_to_string(
            zeroclaw_config::pairing::gateway_admin_token_path(&self.data_dir),
        )
        .expect("the gateway's admin token");
        let reply = self
            .direct(&format!(
                "POST /admin/paircode/new HTTP/1.1\r\nHost: 127.0.0.1\r\n\
                 {}: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                zeroclaw_config::pairing::GATEWAY_ADMIN_TOKEN_HEADER,
                admin_token.trim()
            ))
            .await;
        assert_eq!(reply.status, 200, "{}", reply.text);
        reply.json()["pairing_code"]
            .as_str()
            .unwrap_or_else(|| panic!("a pairing code: {}", reply.text))
            .to_string()
    }

    async fn stop(self) -> DashboardIngress {
        let _ = self.shutdown_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(5), self.server).await;
        self.ingress
    }
}

struct Reply {
    status: u16,
    text: String,
}

impl Reply {
    fn json(&self) -> serde_json::Value {
        let body = self
            .text
            .split_once("\r\n\r\n")
            .map_or("", |(_, body)| body);
        serde_json::from_str(body).unwrap_or_else(|e| panic!("a JSON body ({e}): {}", self.text))
    }

    /// The relay admission's refusals all point the browser at sign-in.
    fn is_relay_refusal(&self) -> bool {
        self.text.contains("through a relay") && self.text.contains("Sign in with a provider")
    }
}

async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, request: &str) -> Reply {
    stream.write_all(request.as_bytes()).await.expect("write");
    stream.flush().await.expect("flush");
    let mut raw = Vec::new();
    tokio::time::timeout(STEP, stream.read_to_end(&mut raw))
        .await
        .expect("the gateway answers and closes")
        .expect("read");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("an HTTP response, got {text:?}"));
    Reply { status, text }
}

fn get(path: &str, host: &str) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
}

/// `POST /pair` with `code`, plus any `extra` header lines.
fn legacy_pair(host: &str, code: &str, extra: &str) -> String {
    format!(
        "POST /pair HTTP/1.1\r\nHost: {host}\r\nX-Pairing-Code: {code}\r\n{extra}\
         Content-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

/// `POST /api/pair` with `code` in the JSON body.
fn api_pair(host: &str, code: &str) -> String {
    let body = serde_json::json!({ "code": code }).to_string();
    format!(
        "POST /api/pair HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// The status of the answer to a WebSocket upgrade, read from the head
/// alone: a successful upgrade leaves the connection open.
async fn upgrade_status<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, request: &str) -> u16 {
    stream.write_all(request.as_bytes()).await.expect("write");
    stream.flush().await.expect("flush");
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    tokio::time::timeout(STEP, async {
        while !head.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).await.expect("read") == 0 {
                break;
            }
            head.push(byte[0]);
        }
    })
    .await
    .expect("the gateway answers the upgrade");
    let head = String::from_utf8_lossy(&head);
    head.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("an HTTP response, got {head:?}"))
}

/// A WebSocket upgrade for `target` (path and query).
fn upgrade(target: &str, host: &str, protocols: &str) -> String {
    format!(
        "GET {target} HTTP/1.1\r\nHost: {host}\r\nConnection: Upgrade\r\n\
         Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {protocols}\r\n\r\n"
    )
}

/// Run `body` on a runtime with room for the gateway's start-up on a debug
/// build's stack, as the gateway's other full-assembly tests do.
fn on_runtime<F, Fut>(body: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()>,
{
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(body());
        })
        .expect("test thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}

#[test]
fn the_admin_routes_answer_a_relay_browser_as_remote() {
    on_runtime(|| async {
        let gateway = Gateway::start().await;
        for path in ["/admin/paircode", "/admin/sop/pending"] {
            let relayed = gateway.via_relay(BROWSER, &get(path, DASHBOARD_HOST)).await;
            assert_eq!(relayed.status, 403, "{path}: {}", relayed.text);
        }
        let relayed = gateway
            .via_relay(BROWSER, &get("/admin/paircode", DASHBOARD_HOST))
            .await;
        assert!(
            relayed.text.contains("restricted to localhost"),
            "{}",
            relayed.text
        );
        // The same route from a socket on this machine gets past the
        // loopback check (and on to the admin-token check).
        let local = gateway.direct(&get("/admin/paircode", "127.0.0.1")).await;
        assert!(
            !local.text.contains("restricted to localhost"),
            "{}",
            local.text
        );
        gateway.stop().await;
    });
}

#[test]
fn a_relay_browser_cannot_claim_a_loopback_address_in_a_forwarded_header() {
    on_runtime(|| async {
        let gateway = Gateway::start().await;
        // A webhook call without a token is a failed authentication, billed
        // to the client.
        let attempt = format!(
            "POST /webhook HTTP/1.1\r\nHost: {DASHBOARD_HOST}\r\n\
             X-Forwarded-For: 127.0.0.1\r\nX-Real-IP: 127.0.0.1\r\nContent-Length: 0\r\n\
             Connection: close\r\n\r\n"
        );
        // The attempt lockout exempts loopback. Keyed on the browser, the
        // failed attempts run into it; keyed on the claimed 127.0.0.1 they
        // never would.
        let mut locked_out = false;
        for _ in 0..=zeroclaw_gateway::auth_rate_limit::MAX_ATTEMPTS {
            let reply = gateway.via_relay(BROWSER, &attempt).await;
            if reply.status == 429 {
                locked_out = true;
                break;
            }
            assert_eq!(reply.status, 401, "{}", reply.text);
        }
        assert!(locked_out, "the browser's failed attempts are limited");
        // The lockout is the browser's own: another browser is not locked.
        let other = gateway.via_relay("198.51.100.4:40000", &attempt).await;
        assert_eq!(other.status, 401, "{}", other.text);
        gateway.stop().await;
    });
}

#[test]
fn a_relay_browser_cannot_pair_and_the_code_still_pairs_directly() {
    on_runtime(|| async {
        let gateway = Gateway::start().await;
        // A valid code typed into a relay-served page is refused before it
        // is read, so the relay cannot redeem it first; the same code still
        // pairs a client on this machine afterwards.
        let code = gateway.mint_pairing_code().await;
        let relayed = gateway
            .via_relay(BROWSER, &legacy_pair(DASHBOARD_HOST, &code, ""))
            .await;
        assert_eq!(relayed.status, 403, "{}", relayed.text);
        assert!(relayed.is_relay_refusal(), "{}", relayed.text);
        let direct = gateway.direct(&legacy_pair("127.0.0.1", &code, "")).await;
        assert_eq!(direct.status, 200, "{}", direct.text);
        assert!(direct.json()["token"].is_string(), "{}", direct.text);

        let code = gateway.mint_pairing_code().await;
        let relayed = gateway
            .via_relay(BROWSER, &api_pair(DASHBOARD_HOST, &code))
            .await;
        assert_eq!(relayed.status, 403, "{}", relayed.text);
        assert!(relayed.is_relay_refusal(), "{}", relayed.text);
        let direct = gateway.direct(&api_pair("127.0.0.1", &code)).await;
        assert_eq!(direct.status, 200, "{}", direct.text);
        assert!(direct.json()["token"].is_string(), "{}", direct.text);

        // Minting a code is refused through the relay too, which would see
        // it, whoever is signed in. Directly the route is there (and asks
        // for credentials).
        for path in [
            "/api/pairing/initiate",
            "/api/devices/some-device/token/rotate",
        ] {
            let request = format!(
                "POST {path} HTTP/1.1\r\nHost: {{host}}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let relayed = gateway
                .via_relay(BROWSER, &request.replace("{host}", DASHBOARD_HOST))
                .await;
            assert_eq!(relayed.status, 403, "{path}: {}", relayed.text);
            assert!(relayed.is_relay_refusal(), "{path}: {}", relayed.text);
            let direct = gateway
                .direct(&request.replace("{host}", "127.0.0.1"))
                .await;
            assert_eq!(direct.status, 401, "{path}: {}", direct.text);
        }
        gateway.stop().await;
    });
}

#[test]
fn a_relay_browsers_pairing_attempts_leave_no_trace_for_that_browser() {
    on_runtime(|| async {
        // One `/pair` per client per minute, so a single attempt billed to
        // the browser would show.
        let gateway =
            Gateway::start_with(|config| config.gateway.pair_rate_limit_per_minute = 1).await;
        let code = gateway.mint_pairing_code().await;
        // More wrong codes than any limiter or lockout allows.
        for _ in 0..=zeroclaw_gateway::auth_rate_limit::MAX_ATTEMPTS {
            for attempt in [
                legacy_pair(DASHBOARD_HOST, "wrong-code", ""),
                api_pair(DASHBOARD_HOST, "wrong-code"),
            ] {
                let relayed = gateway.via_relay(BROWSER, &attempt).await;
                assert_eq!(relayed.status, 403, "{}", relayed.text);
                assert!(relayed.is_relay_refusal(), "{}", relayed.text);
            }
        }
        // The deployment trusts its proxy, so this request from this
        // machine is billed to the browser's address. The pairing rate
        // limit, the auth lockout and the code's failed-attempt lockout all
        // have that browser as new: none of the relayed attempts reached
        // them.
        let browser_ip = BROWSER.split(':').next().unwrap();
        let direct = gateway
            .direct(&legacy_pair(
                "127.0.0.1",
                &code,
                &format!("X-Forwarded-For: {browser_ip}\r\n"),
            ))
            .await;
        assert_eq!(direct.status, 200, "{}", direct.text);
        gateway.stop().await;
    });
}

#[test]
fn a_relay_browser_cannot_present_a_paired_token() {
    on_runtime(|| async {
        let gateway = Gateway::start().await;
        let code = gateway.mint_pairing_code().await;
        let paired = gateway.direct(&legacy_pair("127.0.0.1", &code, "")).await;
        assert_eq!(paired.status, 200, "{}", paired.text);
        let token = paired.json()["token"].as_str().unwrap().to_string();

        let status = |host: &str| {
            format!(
                "GET /api/status HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\n\
                 Connection: close\r\n\r\n"
            )
        };
        let relayed = gateway.via_relay(BROWSER, &status(DASHBOARD_HOST)).await;
        assert_eq!(relayed.status, 401, "{}", relayed.text);
        assert!(relayed.is_relay_refusal(), "{}", relayed.text);
        let direct = gateway.direct(&status("127.0.0.1")).await;
        assert_eq!(direct.status, 200, "{}", direct.text);

        // A WebSocket upgrade with the token in the subprotocol or the query
        // is refused before it upgrades. `/acp` upgrades as soon as the token
        // checks out, so directly the same request gets 101.
        for (target, protocols) in [
            (
                "/acp".to_string(),
                format!("zeroclaw.acp.v1, bearer.{token}"),
            ),
            (format!("/acp?token={token}"), "zeroclaw.acp.v1".to_string()),
            (
                "/ws/chat?agent=main".to_string(),
                format!("zeroclaw.v1, bearer.{token}"),
            ),
        ] {
            let (ours, theirs) = tokio::io::duplex(64 * 1024);
            let conn = DashboardConn::new(
                theirs,
                BROWSER.parse().unwrap(),
                DashboardOrigin::new(DASHBOARD_HOST, 443).unwrap(),
            )
            .unwrap();
            gateway.ingress.deliver(conn).unwrap();
            let relayed = upgrade_status(ours, &upgrade(&target, DASHBOARD_HOST, &protocols)).await;
            assert_eq!(relayed, 401, "{target} {protocols}");
            if target.starts_with("/acp") {
                let socket = tokio::net::TcpStream::connect(gateway.addr).await.unwrap();
                let direct =
                    upgrade_status(socket, &upgrade(&target, "127.0.0.1", &protocols)).await;
                assert_eq!(direct, 101, "{target} {protocols}");
            }
        }
        gateway.stop().await;
    });
}

#[test]
fn a_relay_browser_cannot_present_the_webhook_secret() {
    on_runtime(|| async {
        const SECRET: &str = "relay-test-webhook-secret";
        let gateway =
            Gateway::start_with(|config| config.gateway.webhook_secret = Some(SECRET.into())).await;
        let code = gateway.mint_pairing_code().await;
        let paired = gateway.direct(&legacy_pair("127.0.0.1", &code, "")).await;
        assert_eq!(paired.status, 200, "{}", paired.text);
        let token = paired.json()["token"].as_str().unwrap().to_string();
        let webhook = |host: &str, extra: &str| {
            format!(
                "POST /webhook HTTP/1.1\r\nHost: {host}\r\n{extra}Content-Length: 0\r\n\
                 Connection: close\r\n\r\n"
            )
        };

        // Through the relay the secret is refused at admission, whatever
        // else the request carries.
        let relayed = gateway
            .via_relay(
                BROWSER,
                &webhook(DASHBOARD_HOST, &format!("X-Webhook-Secret: {SECRET}\r\n")),
            )
            .await;
        assert_eq!(relayed.status, 401, "{}", relayed.text);
        assert!(relayed.is_relay_refusal(), "{}", relayed.text);

        // Directly, with the paired token, the same secret passes both
        // checks (the empty body is what's refused), and a wrong one doesn't.
        let direct = gateway
            .direct(&webhook(
                "127.0.0.1",
                &format!("Authorization: Bearer {token}\r\nX-Webhook-Secret: {SECRET}\r\n"),
            ))
            .await;
        assert_eq!(direct.status, 400, "{}", direct.text);
        assert!(direct.text.contains("Invalid JSON body"), "{}", direct.text);
        let wrong = gateway
            .direct(&webhook(
                "127.0.0.1",
                &format!("Authorization: Bearer {token}\r\nX-Webhook-Secret: not-the-secret\r\n"),
            ))
            .await;
        assert_eq!(wrong.status, 401, "{}", wrong.text);
        assert!(wrong.text.contains("X-Webhook-Secret"), "{}", wrong.text);
        gateway.stop().await;
    });
}

#[test]
fn a_relay_connection_serves_only_its_dashboard_host() {
    on_runtime(|| async {
        let gateway = Gateway::start().await;
        for host in [
            "evil.example",
            "127.0.0.1",
            "localhost",
            "abc.relay.example.evil.example",
        ] {
            let reply = gateway.via_relay(BROWSER, &get("/", host)).await;
            assert_eq!(reply.status, 421, "{host}: {}", reply.text);
        }
        let reply = gateway.via_relay(BROWSER, &get("/", DASHBOARD_HOST)).await;
        assert_ne!(reply.status, 421, "{}", reply.text);
        gateway.stop().await;
    });
}

#[test]
fn a_stopped_gateway_takes_no_more_relay_connections() {
    on_runtime(|| async {
        let gateway = Gateway::start().await;
        let ingress = gateway.stop().await;
        assert!(!ingress.is_attached());
        let (_ours, theirs) = tokio::io::duplex(1024);
        let conn = DashboardConn::new(
            theirs,
            BROWSER.parse().unwrap(),
            DashboardOrigin::new(DASHBOARD_HOST, 443).unwrap(),
        )
        .unwrap();
        assert_eq!(ingress.deliver(conn).unwrap_err(), DeliverError::NoGateway);
    });
}
