//! Router-boundary tests for OIDC-only first contact: no client roster,
//! device record, enrollment listener, or pending pairing code.
use super::*;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt as _;
use std::collections::HashMap;
use tower::ServiceExt as _;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, method, path},
};
use zeroclaw_config::pairing::{PairingCodePolicy, PairingGuard};
use zeroclaw_config::schema::{Config, OidcConfig, OidcValidation, PermissionProfileConfig};

struct Fixture {
    _dir: tempfile::TempDir,
    _idp: MockServer,
    state: AppState,
    inbound: Arc<GatewayInboundAuth>,
    audit: Arc<AuditLogger>,
    router: Router,
    csr: String,
    device_key: rcgen::KeyPair,
}

impl Fixture {
    async fn new(allow: bool, service: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let _ = rustls::crypto::ring::default_provider().install_default();
        zeroclaw_tls::ensure_server_materials(&dir.path().join("tls"), &[]).unwrap();
        let idp = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": idp.uri(), "introspection_endpoint": format!("{}/introspect",idp.uri())
            })))
            .mount(&idp)
            .await;
        Mock::given(method("POST")).and(path("/introspect")).and(body_string_contains("token=synthetic-access-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active":true,"token_type":"Bearer", "iss":idp.uri(), "aud":"zeroclaw", "client_id":"desktop", "sub":"new-user", "groups":["users"], "exp":chrono::Utc::now().timestamp()+600
            }))).mount(&idp).await;
        let mut config = Config {
            data_dir: dir.path().to_path_buf(),
            config_path: dir.path().join("config.toml"),
            ..Default::default()
        };
        config.wss.enabled = true;
        // Device admission is NOT tied to this separate listener or a code.
        config.enroll.enabled = false;
        config.gateway.require_pairing = true;
        config.gateway.paired_tokens.clear();
        config.users.clear();
        config.permission_profiles.insert(
            "desktop".into(),
            PermissionProfileConfig {
                admin: false,
                allowed_agents: vec!["*".into()],
                allowed_tools: vec!["*".into()],
                grants: if allow {
                    HashMap::from([
                        (Resource::Sessions, vec![Verb::Create, Verb::Read]),
                        (Resource::Tools, vec![Verb::Execute]),
                    ])
                } else {
                    Default::default()
                },
                ..Default::default()
            },
        );
        config.oidc.insert(
            "corp".into(),
            OidcConfig {
                issuer: idp.uri(),
                audience: "zeroclaw".into(),
                client_id: "gateway".into(),
                client_secret: Some("synthetic-secret".into()),
                validation: OidcValidation::Introspection,
                claim_path: "groups".into(),
                profile_map: HashMap::from([("users".into(), "desktop".into())]),
                interactive_clients: if service {
                    vec![]
                } else {
                    vec!["desktop".into()]
                },
                service_clients: if service {
                    vec!["desktop".into()]
                } else {
                    vec![]
                },
                service_profile_map: if service {
                    HashMap::from([("desktop".into(), "desktop".into())])
                } else {
                    Default::default()
                },
                ..Default::default()
            },
        );
        let mut state = crate::api::tests::test_state(config.clone());
        // Empty token list with pairing disabled generates no code. This guard
        // is never consulted for OIDC, even though the gateway config requires
        // pairing for its independent native-token compatibility path.
        let pairing = Arc::new(PairingGuard::new(false, &[], PairingCodePolicy::default()));
        assert!(pairing.pairing_code().is_none());
        state.pairing = pairing.clone();
        let inbound = Arc::new(GatewayInboundAuth::from_config(&config, pairing.clone()).unwrap());
        let audit =
            AuditLogger::open_shared(config.security.audit.clone(), config.data_dir.clone())
                .unwrap();
        let router = routes(inbound.clone(), Some(audit.clone())).with_state(state.clone());
        let device_key = rcgen::KeyPair::generate().unwrap();
        let params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        let csr = params
            .serialize_request(&device_key)
            .unwrap()
            .pem()
            .unwrap();
        assert!(pairing.pairing_code().is_none());
        Self {
            _dir: dir,
            _idp: idp,
            state,
            inbound,
            audit,
            router,
            csr,
            device_key,
        }
    }

    fn request(&self, token: Option<&str>) -> Request<Body> {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/oidc/corp/certificate")
            .header("host", "localhost")
            .header("content-type", "application/json");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let mut request = request
            .body(Body::from(
                serde_json::json!({"csr_pem":self.csr}).to_string(),
            ))
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))));
        request
    }

    fn ledger(&self) -> CertLedger {
        CertLedger::open(&self.state.config.read().data_dir, Some(self.audit.clone())).unwrap()
    }
}

#[tokio::test]
async fn previously_unknown_human_bootstraps_without_pairing_or_enroll_listener() {
    let f = Fixture::new(true, false).await;
    assert!(f.ledger().list_active().unwrap().is_empty());
    assert!(f.state.config.read().users.is_empty());
    assert!(f.state.pairing.pairing_code().is_none());
    assert!(!f.state.config.read().enroll.enabled);
    let response = f
        .router
        .clone()
        .oneshot(f.request(Some("synthetic-access-token")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let body: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(body["device_id"].as_str().unwrap().starts_with("dev_"));
    assert!(body.get("key_pem").is_none());
    assert!(body.get("pairing_code").is_none());
    assert_eq!(
        zeroclaw_tls::single_cert_pem_sha256_fingerprint(body["ca_chain_pem"].as_str().unwrap())
            .unwrap(),
        zeroclaw_tls::single_cert_pem_sha256_fingerprint(
            &std::fs::read_to_string(f.state.config.read().data_dir.join("tls/ca.crt")).unwrap()
        )
        .unwrap()
    );
    // The certificate created from a previously unknown key is actually
    // admitted by the unchanged mandatory-mTLS builder, not just valid JSON.
    let tls_dir = f.state.config.read().data_dir.join("tls");
    let issued_path = f._dir.path().join("received-client.crt");
    std::fs::write(&issued_path, body["cert_pem"].as_str().unwrap()).unwrap();
    let acceptor = zeroclaw_tls::build_mtls_acceptor(
        tls_dir.join("server.crt").to_str().unwrap(),
        tls_dir.join("server.key").to_str().unwrap(),
        tls_dir.join("ca.crt").to_str().unwrap(),
        &[],
        "",
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = zeroclaw_spawn::spawn!(async move {
        let (stream, _) = listener.accept().await.unwrap();
        acceptor.accept(stream).await.unwrap()
    });
    let mut roots = rustls::RootCertStore::empty();
    for ca in zeroclaw_tls::load_certs(tls_dir.join("ca.crt").to_str().unwrap()).unwrap() {
        roots.add(ca).unwrap();
    }
    let tls = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_client_auth_cert(
            zeroclaw_tls::load_certs(issued_path.to_str().unwrap()).unwrap(),
            rustls::pki_types::PrivateKeyDer::Pkcs8(f.device_key.serialize_der().into()),
        )
        .unwrap();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(tls));
    let tcp = tokio::net::TcpStream::connect(address).await.unwrap();
    let client = connector
        .connect(
            rustls::pki_types::ServerName::try_from("127.0.0.1".to_string()).unwrap(),
            tcp,
        )
        .await
        .unwrap();
    let server = server.await.unwrap();
    assert_eq!(
        client.get_ref().1.protocol_version(),
        Some(rustls::ProtocolVersion::TLSv1_3)
    );
    drop(server);
    let records = f.ledger().list_active().unwrap();
    assert_eq!(records.len(), 1);
    assert!(records[0].actor.starts_with("oidc:oidc:"));
    assert!(records[0].token_hash.is_empty());
    assert_eq!(records[0].device_id, body["device_id"]);
    assert!(
        zeroclaw_runtime::security::audit::verify_chain(
            &f.state.config.read().data_dir.join("audit.log")
        )
        .unwrap()
            >= 2
    );
}

#[tokio::test]
async fn missing_credentials_insufficient_grants_and_service_tokens_cannot_issue() {
    for (allow, service, token, status) in [
        (true, false, None, StatusCode::UNAUTHORIZED),
        (
            true,
            false,
            Some("native-or-invalid-token"),
            StatusCode::UNAUTHORIZED,
        ),
        (
            false,
            false,
            Some("synthetic-access-token"),
            StatusCode::FORBIDDEN,
        ),
        (
            true,
            true,
            Some("synthetic-access-token"),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let f = Fixture::new(allow, service).await;
        let response = f.router.clone().oneshot(f.request(token)).await.unwrap();
        assert_eq!(response.status(), status);
        assert!(f.ledger().list_active().unwrap().is_empty());
    }
}

#[tokio::test]
async fn provider_revocation_static_pins_and_unavailable_audit_fail_closed() {
    let f = Fixture::new(true, false).await;
    let mut cfg = f.state.config.read().clone();
    cfg.oidc.clear();
    f.inbound.publish_persisted(&cfg).unwrap();
    let response = f
        .router
        .clone()
        .oneshot(f.request(Some("synthetic-access-token")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(f.ledger().list_active().unwrap().is_empty());

    let f = Fixture::new(true, false).await;
    let mut pinned = f.state.config.read().clone();
    pinned.wss.client_auth = Some(zeroclaw_config::schema::WssClientAuthConfig {
        pinned_certs: vec!["test-static-pin".into()],
        ..Default::default()
    });
    let state = crate::api::tests::test_state(pinned);
    let router = routes(f.inbound.clone(), Some(f.audit.clone())).with_state(state);
    let response = router
        .oneshot(f.request(Some("synthetic-access-token")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(f.ledger().list_active().unwrap().is_empty());

    let f = Fixture::new(true, false).await;
    let no_audit = routes(f.inbound.clone(), None).with_state(f.state.clone());
    let response = no_audit
        .oneshot(f.request(Some("synthetic-access-token")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(f.ledger().list_active().unwrap().is_empty());
}

#[tokio::test]
async fn provider_revoked_during_verification_cannot_publish_a_certificate() {
    let f = Fixture::new(true, false).await;
    let began = Arc::new(tokio::sync::Notify::new());
    let notify = began.clone();
    Mock::given(method("POST")).and(path("/introspect"))
        .respond_with(move |_: &wiremock::Request| {
            notify.notify_one();
            ResponseTemplate::new(200).set_delay(std::time::Duration::from_millis(100))
                .set_body_json(serde_json::json!({
                    "active":true,"token_type":"Bearer", "aud":"zeroclaw", "client_id":"desktop", "sub":"new-user", "groups":["users"], "exp":chrono::Utc::now().timestamp()+600
                }))
        }).with_priority(1).mount(&f._idp).await;
    let router = f.router.clone();
    let request = f.request(Some("synthetic-access-token"));
    let result = zeroclaw_spawn::spawn!(async move { router.oneshot(request).await.unwrap() });
    began.notified().await;
    let mut revoked = f.state.config.read().clone();
    revoked.oidc.clear();
    f.inbound.publish_persisted(&revoked).unwrap();
    let response = result.await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(f.ledger().list_active().unwrap().is_empty());
}

#[tokio::test]
async fn remote_cleartext_cannot_obtain_a_device_even_with_a_valid_bearer() {
    let f = Fixture::new(true, false).await;
    let mut request = f.request(Some("synthetic-access-token"));
    request
        .headers_mut()
        .insert(header::HOST, "daemon.example".parse().unwrap());
    request
        .headers_mut()
        .insert("x-forwarded-proto", "https".parse().unwrap());
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([203, 0, 113, 9], 12345))));
    let response = f.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        f._idp.received_requests().await.unwrap().is_empty(),
        "must refuse before disclosing bearer to IdP"
    );
    assert!(f.ledger().list_active().unwrap().is_empty());
}

#[tokio::test]
async fn session_creator_needs_an_agent_selector_to_bootstrap() {
    let f = Fixture::new(true, false).await;
    let mut config = f.state.config.read().clone();
    config
        .permission_profiles
        .get_mut("desktop")
        .unwrap()
        .allowed_agents
        .clear();
    f.inbound.publish_persisted(&config).unwrap();
    let response = f
        .router
        .clone()
        .oneshot(f.request(Some("synthetic-access-token")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(f.ledger().list_active().unwrap().is_empty());
}
