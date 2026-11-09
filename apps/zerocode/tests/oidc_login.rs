//! Process-boundary proof for URL-scoped login. The client and both transports
//! are real; loopback peers implement the gateway/RPC wire contracts with
//! synthetic credentials. No production accounts or identity provider needed.

// Standalone client tests have no daemon attribution span, as in main.rs.
#![allow(clippy::disallowed_methods)]

use std::process::{Output, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const TOKEN: &str = "synthetic-oidc-access-token-never-print";

struct Fixture {
    dir: tempfile::TempDir,
    target: String,
    gateway: MockServer,
    tls_task: tokio::task::JoinHandle<()>,
    initializes: Arc<AtomicUsize>,
    auth_rejections: Arc<AtomicUsize>,
    enroll_port: u16,
    enroll_task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.tls_task.abort();
        self.enroll_task.abort();
    }
}

impl Fixture {
    async fn new(reject: bool) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = tempfile::tempdir().unwrap();
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = params.self_signed(&ca_key).unwrap();
        let server_key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["127.0.0.1".into()]).unwrap();
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let server = params.signed_by(&server_key, &ca, &ca_key).unwrap();
        let client_key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
        let client = params.signed_by(&client_key, &ca, &ca_key).unwrap();
        for (file, pem) in [
            ("ca.pem", ca.pem()),
            ("client.pem", client.pem()),
            ("client.key", client_key.serialize_pem()),
        ] {
            std::fs::write(dir.path().join(file), pem).unwrap();
        }
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca.der().clone()).unwrap();
        let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .unwrap();
        let config = rustls::ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(
                vec![server.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(
                    rustls::pki_types::PrivatePkcs8KeyDer::from(server_key.serialize_der()),
                ),
            )
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = format!("wss://{}/rpc", listener.local_addr().unwrap());
        let initializes = Arc::new(AtomicUsize::new(0));
        let counter = initializes.clone();
        let auth_rejections = Arc::new(AtomicUsize::new(0));
        let reject_counter = auth_rejections.clone();
        let tls_task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let acceptor = acceptor.clone();
                let counter = counter.clone();
                let reject_counter = reject_counter.clone();
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(stream).await else {
                        return;
                    };
                    // The credential-free TLS preflight closes before HTTP.
                    let Ok(mut ws) = tokio_tungstenite::accept_async(tls).await else {
                        return;
                    };
                    let frame = ws.next().await.unwrap().unwrap();
                    let request: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
                    assert_eq!(request["method"], "initialize");
                    assert_eq!(request["params"]["auth_token"], TOKEN);
                    assert_eq!(request["params"]["auth_provider"], "oidc.corp");
                    counter.fetch_add(1, Ordering::SeqCst);
                    let reject_once = reject_counter
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                            remaining.checked_sub(1)
                        })
                        .is_ok();
                    let response = if reject || reject_once {
                        json!({"jsonrpc":"2.0", "id":request["id"], "error":{"code":-32010,"message":"authentication refused"}})
                    } else {
                        json!({"jsonrpc":"2.0", "id":request["id"], "result":{"server_version":env!("CARGO_PKG_VERSION"),"commands":[]}})
                    };
                    ws.send(Message::Text(response.to_string().into()))
                        .await
                        .unwrap();
                    let _ = ws.next().await;
                });
            }
        });
        let enroll_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let enroll_port = enroll_listener.local_addr().unwrap().port();
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(
            rustls::pki_types::PrivatePkcs8KeyDer::from(server_key.serialize_der()),
        );
        let cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![server.der().clone()], key)
            .unwrap();
        let enroll_acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));
        let enroll_task = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            loop {
                let (stream, _) = enroll_listener.accept().await.unwrap();
                let Ok(mut tls) = enroll_acceptor.accept(stream).await else {
                    continue;
                };
                let mut raw = Vec::new();
                let end = loop {
                    let mut byte = [0u8; 1];
                    if tls.read_exact(&mut byte).await.is_err() {
                        return;
                    }
                    raw.push(byte[0]);
                    if raw.ends_with(b"\r\n\r\n") {
                        break raw.len();
                    }
                };
                let header = String::from_utf8_lossy(&raw).to_string();
                let response = if header.starts_with("GET /enroll/ca ") {
                    json!({"ca_chain_pem": ca.pem()})
                } else {
                    assert!(header.starts_with("POST /enroll "));
                    let length: usize = header.lines().find_map(|line| line.strip_prefix("Content-Length: ")).unwrap().trim().parse().unwrap();
                    raw.resize(end + length, 0);
                    tls.read_exact(&mut raw[end..]).await.unwrap();
                    let request: Value = serde_json::from_slice(&raw[end..]).unwrap();
                    assert_eq!(request["pairing_code"], "test-enrollment-code");
                    let mut csr = rcgen::CertificateSigningRequestParams::from_pem(request["csr_pem"].as_str().unwrap()).unwrap();
                    csr.params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
                    let cert = csr.signed_by(&ca, &ca_key).unwrap();
                    json!({"cert_pem":cert.pem(),"ca_chain_pem":ca.pem(),"device_id":"test-device", "not_after":chrono::Utc::now().timestamp()+2592000})
                }.to_string();
                tls.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).as_bytes()).await.unwrap();
                tls.shutdown().await.unwrap();
            }
        });
        let gateway = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/oidc/corp/device/start"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code":"synthetic-device-code", "user_code":"ABCD-EFGH",
                "verification_uri":"https://idp.example/activate", "expires_in":600,"interval":5
            })))
            .mount(&gateway)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/oidc/corp/device/poll"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"status":"granted","token":{"access_token":TOKEN,"expires_in":3600}}),
            ))
            .mount(&gateway)
            .await;
        Self {
            dir,
            target,
            gateway,
            tls_task,
            initializes,
            auth_rejections,
            enroll_port,
            enroll_task,
        }
    }

    fn command(&self) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_zerocode"));
        // Keep the test independent of a developer's transient credentials.
        for (key, _) in std::env::vars()
            .filter(|(key, _)| key.starts_with("ZEROCODE_") || key == "ZEROCLAW_AUTH_TOKEN")
        {
            cmd.env_remove(key);
        }
        cmd.args([
            "login",
            "corp",
            &self.target,
            "--gateway-url",
            &self.gateway.uri(),
        ])
        .arg("--config-dir")
        .arg(self.dir.path())
        .arg("--tls-ca-cert")
        .arg(self.dir.path().join("ca.pem"))
        .arg("--tls-client-cert")
        .arg(self.dir.path().join("client.pem"))
        .arg("--tls-client-key")
        .arg(self.dir.path().join("client.key"))
        .stdin(Stdio::null())
        .kill_on_drop(true);
        cmd
    }

    async fn run(&self) -> Output {
        tokio::time::timeout(Duration::from_secs(25), self.command().output())
            .await
            .unwrap()
            .unwrap()
    }

    fn config(&self) -> std::path::PathBuf {
        self.dir.path().join("zerocode-config.toml")
    }
}

#[tokio::test]
async fn login_validates_remote_principal_then_persists_without_printing_token() {
    let f = Fixture::new(false).await;
    let out = f.run().await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(out.stdout.is_empty());
    assert!(!stderr.contains(TOKEN));
    assert!(stderr.contains("Saved the login"), "{stderr}");
    assert_eq!(f.initializes.load(Ordering::SeqCst), 1);
    let config: toml::Value =
        toml::from_str(&std::fs::read_to_string(f.config()).unwrap()).unwrap();
    assert_eq!(
        config["connection"]["wss"]["uri"].as_str(),
        Some(f.target.as_str())
    );
    let login = &config["connection"]["logins"][&f.target];
    assert_eq!(login["auth_token"].as_str(), Some(TOKEN));
    assert_eq!(login["auth_provider"].as_str(), Some("oidc.corp"));
    assert_eq!(login["enroll_url"].as_str(), Some(f.gateway.uri().as_str()));
    assert!(login["expires_at"].as_integer().unwrap() > chrono::Utc::now().timestamp());
    assert!(
        !login["tls"]
            .get("skip_verify")
            .and_then(toml::Value::as_bool)
            .unwrap_or(false)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(f.config()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn refused_initialize_preserves_previous_file() {
    let f = Fixture::new(true).await;
    let before = "[theme]\nname = 'zeroclaw'\n[future]\nkeep = 'unchanged'\n";
    std::fs::write(f.config(), before).unwrap();
    let out = f.run().await;
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains(TOKEN));
    assert_eq!(f.initializes.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read_to_string(f.config()).unwrap(), before);
}

#[tokio::test]
async fn untrusted_certificate_and_eof_abort_before_device_start() {
    let f = Fixture::new(false).await;
    // Use the publicly rooted default instead of the fixture CA. No stdin
    // means no insecure consent; the command must never start a device grant.
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_zerocode"));
    cmd.args([
        "login",
        "corp",
        &f.target,
        "--gateway-url",
        &f.gateway.uri(),
    ])
    .arg("--config-dir")
    .arg(f.dir.path())
    .arg("--tls-client-cert")
    .arg(f.dir.path().join("client.pem"))
    .arg("--tls-client-key")
    .arg(f.dir.path().join("client.key"))
    .stdin(Stdio::null())
    .env_remove("ZEROCLAW_AUTH_TOKEN")
    .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(10), cmd.output())
        .await
        .unwrap()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("No login was saved"));
    assert!(f.gateway.received_requests().await.unwrap().is_empty());
    assert_eq!(f.initializes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn first_login_enrolls_device_and_plain_zerocode_reuses_saved_credentials() {
    use tokio::io::AsyncWriteExt as _;
    let f = Fixture::new(false).await;
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_zerocode"));
    cmd.args([
        "login",
        "corp",
        &f.target,
        "--gateway-url",
        &f.gateway.uri(),
    ])
    .arg("--enroll-port")
    .arg(f.enroll_port.to_string())
    .arg("--config-dir")
    .arg(f.dir.path())
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    for (key, _) in std::env::vars()
        .filter(|(key, _)| key.starts_with("ZEROCODE_") || key == "ZEROCLAW_AUTH_TOKEN")
    {
        cmd.env_remove(key);
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"test-enrollment-code\ny\n")
        .await
        .unwrap();
    let out = tokio::time::timeout(Duration::from_secs(25), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!String::from_utf8_lossy(&out.stderr).contains(TOKEN));
    assert_eq!(f.initializes.load(Ordering::SeqCst), 1);
    let config: toml::Value =
        toml::from_str(&std::fs::read_to_string(f.config()).unwrap()).unwrap();
    let cert = config["connection"]["logins"][&f.target]["tls"]["client_cert_path"]
        .as_str()
        .unwrap();
    assert!(std::path::Path::new(cert).exists());
    assert!(cert.contains("connections"));

    // No --connect, --enroll, TLS or bearer flags: observe the real initialize
    // before terminal initialization (there is deliberately no TTY in this test).
    let mut plain = tokio::process::Command::new(env!("CARGO_BIN_EXE_zerocode"));
    plain
        .arg("--config-dir")
        .arg(f.dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for (key, _) in std::env::vars()
        .filter(|(key, _)| key.starts_with("ZEROCODE_") || key == "ZEROCLAW_AUTH_TOKEN")
    {
        plain.env_remove(key);
    }
    let mut child = plain.spawn().unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while f.initializes.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("bare zerocode initializes using the saved login");
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
async fn startup_renews_expired_token_and_retries_a_rejected_login_without_token_flags() {
    let f = Fixture::new(false).await;
    let out = f.run().await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    for expired in [true, false] {
        let before_count = f.initializes.load(Ordering::SeqCst);
        let mut config: toml::Value =
            toml::from_str(&std::fs::read_to_string(f.config()).unwrap()).unwrap();
        config["connection"]["logins"][&f.target]["expires_at"] =
            toml::Value::Integer(if expired {
                1
            } else {
                chrono::Utc::now().timestamp() + 3600
            });
        std::fs::write(f.config(), toml::to_string(&config).unwrap()).unwrap();
        if !expired {
            f.auth_rejections.store(1, Ordering::SeqCst);
        }
        let expected_count = before_count + if expired { 2 } else { 3 };
        let mut plain = tokio::process::Command::new(env!("CARGO_BIN_EXE_zerocode"));
        plain
            .arg("--config-dir")
            .arg(f.dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for (key, _) in std::env::vars()
            .filter(|(key, _)| key.starts_with("ZEROCODE_") || key == "ZEROCLAW_AUTH_TOKEN")
        {
            plain.env_remove(key);
        }
        let mut child = plain.spawn().unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            while f.initializes.load(Ordering::SeqCst) < expected_count {
                if let Some(status) = child.try_wait().unwrap() {
                    panic!("zerocode exited before renewing login: {status}");
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("startup re-authenticates without manual token handling");
        let _ = child.kill().await;
        let _ = child.wait().await;
        let config: toml::Value =
            toml::from_str(&std::fs::read_to_string(f.config()).unwrap()).unwrap();
        assert!(
            config["connection"]["logins"][&f.target]["expires_at"]
                .as_integer()
                .unwrap()
                > chrono::Utc::now().timestamp()
        );
    }
}
