//! The real gateway serving connections a relay-served dashboard forwarded.
//!
//! Each test starts `run_gateway_with_plugin_webhooks`, with every route
//! mounted, on a loopback port, attached to a [`DashboardIngress`] the test
//! owns. It then hands the gateway in-memory connections exactly as the
//! relay bridge does, and compares what they get with what a socket on this
//! machine gets. The property under test is that a relay-forwarded browser is
//! never this machine: not to the admin routes, not to the limiters, and not
//! by claiming a loopback address in a forwarded header.

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
        let root = tempfile::TempDir::new().expect("temp root");
        let config = config(root.path());
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
        let attempt = format!(
            "POST /pair HTTP/1.1\r\nHost: {DASHBOARD_HOST}\r\nX-Pairing-Code: 00000000\r\n\
             X-Forwarded-For: 127.0.0.1\r\nX-Real-IP: 127.0.0.1\r\nContent-Length: 0\r\n\
             Connection: close\r\n\r\n"
        );
        // The attempt lockout exempts loopback. Keyed on the browser, the
        // wrong codes run into it; keyed on the claimed 127.0.0.1 they never
        // would.
        let mut locked_out = false;
        for _ in 0..=zeroclaw_gateway::auth_rate_limit::MAX_ATTEMPTS {
            let reply = gateway.via_relay(BROWSER, &attempt).await;
            if reply.status == 429 {
                locked_out = true;
                break;
            }
            assert_eq!(reply.status, 403, "{}", reply.text);
        }
        assert!(locked_out, "the browser's wrong pairing codes are limited");
        // The lockout is the browser's own: another browser is not locked.
        let other = gateway.via_relay("198.51.100.4:40000", &attempt).await;
        assert_eq!(other.status, 403, "{}", other.text);
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
