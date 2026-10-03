//! Two processes: the core's real local listener in this test process, and
//! the preview `zeroclaw-gw` binary as a child that reaches it only over the
//! socket. Each side is stopped and started again while the other keeps
//! running. Unix only: the preview refuses to start elsewhere.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;
use zeroclaw_runtime::rpc::context::RpcContext;

const TOKEN: &str = "zc_two_process_token";
const WAIT: Duration = Duration::from_secs(30);

/// The core: the daemon's real local listener, serving a socket under a
/// temporary data directory.
struct Core {
    ctx: Arc<RpcContext>,
    endpoint: PathBuf,
    cancel: CancellationToken,
    listener: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Core {
    fn context(dir: &Path) -> Arc<RpcContext> {
        let mut config = zeroclaw_config::schema::Config {
            data_dir: dir.to_path_buf(),
            config_path: dir.join("config.toml"),
            ..Default::default()
        };
        config.gateway.paired_tokens = vec![TOKEN.into()];
        let sessions = Arc::new(zeroclaw_runtime::rpc::session::SessionStore::new(
            16,
            Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
                4, 10, 60,
            )),
        ));
        RpcContext::for_live_test(config, sessions)
    }

    async fn serve(ctx: Arc<RpcContext>) -> Self {
        let endpoint = zeroclaw_runtime::rpc::local::socket_path(&ctx.config.read());
        let cancel = CancellationToken::new();
        let listener = {
            let ctx = Arc::clone(&ctx);
            let cancel = cancel.clone();
            zeroclaw_spawn::spawn!(async move {
                zeroclaw_runtime::rpc::local::run_local_listener(
                    ctx,
                    cancel,
                    Arc::new(AtomicUsize::new(0)),
                    None,
                )
                .await
            })
        };
        let reachable = tokio::time::timeout(WAIT, async {
            while tokio::net::UnixStream::connect(&endpoint).await.is_err() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(reachable.is_ok(), "the core never listened");
        Self {
            ctx,
            endpoint,
            cancel,
            listener,
        }
    }

    async fn stop(self) -> Arc<RpcContext> {
        self.cancel.cancel();
        let stopped = tokio::time::timeout(WAIT, self.listener)
            .await
            .expect("the core stops");
        stopped
            .expect("the core's listener task")
            .expect("the core's listener");
        self.ctx
    }
}

/// The preview gateway as a child process.
struct Gateway {
    child: Child,
    address: String,
}

impl Gateway {
    async fn spawn(endpoint: &Path, extra: &[&str]) -> Self {
        Self::start(endpoint, extra, Stdio::inherit()).await
    }

    /// [`Gateway::spawn`], collecting the lines it writes on stderr, its log.
    async fn spawn_logged(endpoint: &Path, extra: &[&str]) -> (Self, Arc<Mutex<Vec<String>>>) {
        let mut gateway = Self::start(endpoint, extra, Stdio::piped()).await;
        let stderr = gateway.child.stderr.take().expect("piped stderr");
        let log = Arc::new(Mutex::new(Vec::new()));
        {
            let log = Arc::clone(&log);
            zeroclaw_spawn::spawn!(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    log.lock().unwrap().push(line);
                }
            });
        }
        (gateway, log)
    }

    async fn start(endpoint: &Path, extra: &[&str], stderr: Stdio) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_zeroclaw-gw"))
            .arg("--listen")
            .arg("127.0.0.1:0")
            .arg("--socket")
            .arg(endpoint)
            .args(extra)
            .env_remove("ZEROCLAW_SOCKET")
            .stdout(Stdio::piped())
            .stderr(stderr)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn zeroclaw-gw");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut line = String::new();
        tokio::time::timeout(WAIT, BufReader::new(stdout).read_line(&mut line))
            .await
            .expect("zeroclaw-gw reports readiness")
            .expect("read its stdout");
        let url = line
            .trim()
            .strip_prefix("READY ")
            .unwrap_or_else(|| panic!("unexpected first line: {line:?}"));
        let address = url
            .split_once("://")
            .map(|(_, address)| address.to_owned())
            .expect("a URL");
        Self { child, address }
    }

    async fn kill(mut self) {
        self.child.kill().await.expect("kill zeroclaw-gw");
    }
}

/// A plain HTTP/1.1 GET; returns the status and the body as JSON.
async fn get(address: &str, path: &str, token: Option<&str>) -> (u16, Value) {
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect to zeroclaw-gw");
    let auth = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {address}\r\n{auth}Connection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("send");
    let mut response = Vec::new();
    tokio::time::timeout(WAIT, stream.read_to_end(&mut response))
        .await
        .expect("a response in time")
        .expect("read the response");
    let text = String::from_utf8_lossy(&response);
    let (head, body) = text.split_once("\r\n\r\n").expect("headers and body");
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("a status code");
    let body = serde_json::from_str(body).unwrap_or_else(|_| Value::String(body.to_owned()));
    (status, body)
}

/// Poll `path` until it answers `status`.
async fn until_status(address: &str, path: &str, token: Option<&str>, status: u16) -> Value {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let (actual, body) = get(address, path, token).await;
        if actual == status {
            return body;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{path} never answered {status}; last {actual}: {body}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn the_separate_gateway_follows_the_core_through_restarts_of_either() {
    let tmp = tempfile::tempdir().unwrap();
    let core = Core::serve(Core::context(tmp.path())).await;
    let endpoint = core.endpoint.clone();
    let gateway = Gateway::spawn(&endpoint, &[]).await;

    let (status, health) = get(&gateway.address, "/health", None).await;
    assert_eq!(status, 200, "{health}");
    assert_eq!(health["core"]["link"], "reachable");
    let (status, link) = get(&gateway.address, "/api/gateway/core", Some(TOKEN)).await;
    assert_eq!(status, 200, "{link}");
    assert_eq!(link["principal_id"], "shared-operator");
    let (status, refused) = get(&gateway.address, "/api/gateway/core", Some("zc_forged")).await;
    assert_eq!(status, 401, "{refused}");
    assert_eq!(refused["code"], "auth_required");
    assert!(refused["hint"].is_string(), "{refused}");
    let (status, refused) = get(&gateway.address, "/api/status", Some(TOKEN)).await;
    assert_eq!(status, 503, "{refused}");
    assert_eq!(refused["code"], "capability_missing");

    // A ported dashboard route is served through the core. The gateway's own
    // connection, which the core registers like any client's, declared
    // itself a gateway's and is not listed as a terminal.
    let (status, health) = get(&gateway.address, "/api/health", Some(TOKEN)).await;
    assert_eq!(status, 200, "{health}");
    assert!(health["health"]["components"].is_object(), "{health}");
    let (status, tuis) = get(&gateway.address, "/api/tuis", Some(TOKEN)).await;
    assert_eq!(status, 200, "{tuis}");
    assert_eq!(tuis["tuis"], serde_json::json!([]), "{tuis}");
    let registered = core.ctx.tui_registry.list();
    assert!(
        registered
            .iter()
            .any(|tui| tui.peer_label.starts_with("unix:")
                && tui.client_kind.as_deref() == Some("gateway")),
        "the core registers the separate gateway's connection as a gateway's: {:?}",
        registered
            .iter()
            .map(|tui| (&tui.peer_label, &tui.client_kind))
            .collect::<Vec<_>>()
    );

    // The core stops: the gateway keeps running and says so.
    let ctx = core.stop().await;
    let health = until_status(&gateway.address, "/health", None, 503).await;
    assert_eq!(health["code"], "core_unavailable");
    let (status, down) = get(&gateway.address, "/api/gateway/core", Some(TOKEN)).await;
    assert_eq!(status, 503, "{down}");
    assert_eq!(down["code"], "core_unavailable");

    // The core comes back: the gateway reconnects on the next request.
    let core = Core::serve(ctx).await;
    let link = until_status(&gateway.address, "/api/gateway/core", Some(TOKEN), 200).await;
    assert_eq!(link["principal_id"], "shared-operator");

    // The gateway dies: the core keeps serving its other clients.
    gateway.kill().await;
    let client = zeroclaw_rpc_client::RpcClient::connect_local(
        &endpoint,
        zeroclaw_rpc_client::ConnectOptions {
            auth_token: Some(TOKEN.into()),
            ..Default::default()
        },
    )
    .await
    .expect("the core still accepts clients");
    client
        .request(zeroclaw_rpc_client::Method::Status, serde_json::json!({}))
        .await
        .expect("the core still serves");
    drop(client);

    // A new gateway picks up where the old one stopped.
    let gateway = Gateway::spawn(&endpoint, &[]).await;
    let (status, link) = get(&gateway.address, "/api/gateway/core", Some(TOKEN)).await;
    assert_eq!(status, 200, "{link}");

    gateway.kill().await;
    core.stop().await;
}

/// A core of `version` that answers the handshake and `status` and nothing
/// else, counting the requests past the handshake it is sent.
fn scripted_core(endpoint: &Path, version: &'static str) -> Arc<AtomicUsize> {
    use std::sync::atomic::Ordering;
    let listener = tokio::net::UnixListener::bind(endpoint).expect("bind the scripted core");
    let past_handshake = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&past_handshake);
    zeroclaw_spawn::spawn!(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let counter = Arc::clone(&counter);
            zeroclaw_spawn::spawn!(async move {
                let (read, mut write) = tokio::io::split(stream);
                let mut lines = BufReader::new(read).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let frame: Value = serde_json::from_str(&line).expect("a JSON frame");
                    let result = if frame["method"] == "initialize" {
                        serde_json::json!({
                            "protocol_version": 1, "server_version": version,
                            "server_pid": 1, "principal_id": "shared-operator",
                        })
                    } else {
                        counter.fetch_add(1, Ordering::SeqCst);
                        serde_json::json!({ "server_version": version, "protocol_version": 1 })
                    };
                    let answer = serde_json::json!({ "jsonrpc": "2.0", "id": frame["id"], "result": result });
                    if write
                        .write_all(format!("{answer}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
    });
    past_handshake
}

/// Wait until `log` holds a line containing `needle`; return how many do.
async fn logged(log: &Mutex<Vec<String>>, needle: &str) -> usize {
    let count = || {
        log.lock()
            .unwrap()
            .iter()
            .filter(|line| line.contains(needle))
            .count()
    };
    let deadline = tokio::time::Instant::now() + WAIT;
    while count() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "zeroclaw-gw never logged {needle:?}: {:?}",
            log.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Lines written alongside the first one have had time to arrive.
    tokio::time::sleep(Duration::from_millis(200)).await;
    count()
}

#[tokio::test]
async fn the_separate_gateway_refuses_a_core_of_another_version_unless_skew_is_allowed() {
    use std::sync::atomic::Ordering;
    const SKEWED: &str = "0.0.0-skewed";
    let tmp = tempfile::tempdir().unwrap();
    let endpoint = tmp.path().join("daemon.sock");
    let past_handshake = scripted_core(&endpoint, SKEWED);

    // By default: refused, with both versions named, logged once.
    let (gateway, log) = Gateway::spawn_logged(&endpoint, &[]).await;
    let (status, health) = get(&gateway.address, "/health", None).await;
    assert_eq!(
        status, 200,
        "the gateway's own health still answers: {health}"
    );
    for path in ["/api/gateway/core", "/api/health", "/api/gateway/core"] {
        let (status, refused) = get(&gateway.address, path, Some(TOKEN)).await;
        assert_eq!(status, 503, "{path}: {refused}");
        assert_eq!(refused["code"], "core_version_mismatch", "{path}");
        assert_eq!(refused["versions"]["core"], SKEWED, "{path}");
        assert_eq!(
            refused["versions"]["gateway"],
            env!("CARGO_PKG_VERSION"),
            "{path}"
        );
    }
    assert_eq!(
        past_handshake.load(Ordering::SeqCst),
        0,
        "nothing past the handshake reached the refused core"
    );
    assert_eq!(
        logged(&log, "core_version_mismatch").await,
        1,
        "one log line per refused core version, not one per request"
    );
    gateway.kill().await;

    // With the development flag: served, and the flag is announced.
    let (gateway, log) = Gateway::spawn_logged(&endpoint, &["--allow-version-skew"]).await;
    let (status, link) = get(&gateway.address, "/api/gateway/core", Some(TOKEN)).await;
    assert_eq!(status, 200, "{link}");
    assert_eq!(link["core"]["server_version"], SKEWED);
    assert!(past_handshake.load(Ordering::SeqCst) > 0);
    assert_eq!(logged(&log, "--allow-version-skew").await, 1);
    gateway.kill().await;
}

#[tokio::test]
async fn without_an_endpoint_it_refuses_to_start() {
    let output = Command::new(env!("CARGO_BIN_EXE_zeroclaw-gw"))
        .env_remove("ZEROCLAW_SOCKET")
        .output()
        .await
        .expect("run zeroclaw-gw");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("config.toml"), "{stderr}");
}

#[tokio::test]
async fn with_tls_flags_it_serves_https() {
    let tmp = tempfile::tempdir().unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["localhost".into()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let cert_path = tmp.path().join("cert.pem");
    let key_path = tmp.path().join("key.pem");
    std::fs::write(&cert_path, cert.pem()).unwrap();
    std::fs::write(&key_path, key.serialize_pem()).unwrap();

    let core = Core::serve(Core::context(tmp.path())).await;
    let gateway = Gateway::spawn(
        &core.endpoint,
        &[
            "--tls-cert",
            cert_path.to_str().unwrap(),
            "--tls-key",
            key_path.to_str().unwrap(),
        ],
    )
    .await;
    // A plain HTTP request to the TLS listener gets no HTTP response.
    let mut stream = tokio::net::TcpStream::connect(&gateway.address)
        .await
        .expect("the TLS listener accepts");
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .expect("send");
    let mut response = Vec::new();
    let _ = tokio::time::timeout(WAIT, stream.read_to_end(&mut response)).await;
    assert!(
        !response.starts_with(b"HTTP/1.1 200"),
        "the listener speaks TLS, not plain HTTP"
    );

    gateway.kill().await;
    core.stop().await;
}
