//! Real Component Model coverage for the plugin WebSocket resource.
//!
//! A compiled tool component (`tests/fixtures/tool-websocket-fixture`) opens a
//! host WebSocket through the tool runtime exactly as a deployed plugin would,
//! against loopback `ws://` and `wss://` echo servers. Reach comes only from the
//! egress grant (ADR-014): the destination must be in `egress_hosts`, `ws://`
//! needs no separate exception, and a TLS profile selects certificates without
//! granting anything.

#![cfg(feature = "plugins-wasm-cranelift")]

mod support;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, OnceLock};

use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use zeroclaw_api::plugin_key::SecretPropertyRef;
use zeroclaw_plugins::component::PluginLimits;
use zeroclaw_plugins::config::{PluginConfigResolver, resolve_plugin_config};
use zeroclaw_plugins::egress::{
    EgressHostService, EgressPolicy, EgressPolicyResolver, TlsProfile, TlsProfileName,
};
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::runtime;
use zeroclaw_plugins::services::PluginHostServices;
use zeroclaw_plugins::{PluginCapability, PluginManifest, PluginPermission};

use support::{admit_fixture, state_service};

fn fixture() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/tool-websocket-fixture");
            let target_dir =
                PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("tool-websocket-plugin-fixture");
            let status = Command::new(env!("CARGO"))
                .current_dir(&fixture_dir)
                .args([
                    "build",
                    "--locked",
                    "--quiet",
                    "--package",
                    "zeroclaw-tool-websocket-plugin-fixture",
                    "--target",
                    "wasm32-wasip2",
                    "--target-dir",
                ])
                .arg(&target_dir)
                .status()
                .expect("run Cargo for the tool websocket component fixture");
            assert!(
                status.success(),
                "tool websocket fixture must build; install the wasm32-wasip2 target"
            );
            let wasm =
                target_dir.join("wasm32-wasip2/debug/zeroclaw_tool_websocket_plugin_fixture.wasm");
            assert!(
                wasm.is_file(),
                "tool websocket fixture WASM was not produced"
            );
            wasm
        })
        .clone()
}

fn limits() -> PluginLimits {
    PluginLimits {
        call_fuel: 1_000_000_000,
        max_memory_bytes: 64 * 1024 * 1024,
        max_table_elements: 10_000,
        max_instances: 10,
        call_timeout: std::time::Duration::from_secs(20),
    }
}

fn manifest() -> PluginManifest {
    PluginManifest {
        name: "tool-websocket-fixture".to_string(),
        version: "0.0.0".to_string(),
        description: None,
        author: None,
        wasm_path: Some("tool-websocket-fixture.wasm".to_string()),
        wasm_sha256: None,
        capabilities: vec![PluginCapability::Tool],
        permissions: vec![
            PluginPermission::ConfigRead,
            PluginPermission::WebSocketClient,
        ],
        config_schema: Some(serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "url": {"type": "string"},
                "profile": {"type": "string"},
                "ca_pem": {"type": "string", "x-secret": true}
            }
        })),
        signature: None,
        publisher_key: None,
        egress: Default::default(),
    }
}

/// One loopback test PKI: a CA, and a `localhost` server certificate it signed.
struct TestPki {
    ca_pem: String,
    server_der: rustls::pki_types::CertificateDer<'static>,
    server_key: PrivateKeyDer<'static>,
}

impl TestPki {
    fn new() -> Self {
        let ca_key = rcgen::KeyPair::generate().expect("CA key");
        let mut ca_params =
            rcgen::CertificateParams::new(vec!["WebSocket E2E CA".to_string()]).expect("CA params");
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).expect("self-sign CA");
        let server_key = rcgen::KeyPair::generate().expect("server key");
        let server = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .expect("server params")
            .signed_by(&server_key, &ca, &ca_key)
            .expect("sign server");
        Self {
            ca_pem: ca.pem(),
            server_der: server.der().clone(),
            server_key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server_key.serialize_der())),
        }
    }

    fn acceptor(&self) -> tokio_rustls::TlsAcceptor {
        tokio_rustls::TlsAcceptor::from(Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![self.server_der.clone()], self.server_key.clone_key())
                .expect("TLS server config"),
        ))
    }
}

/// Accept one WebSocket upgrade on `stream` and echo its text messages.
async fn echo<S>(stream: S)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    while let Some(Ok(message)) = socket.next().await {
        if let Message::Text(text) = message
            && socket.send(Message::Text(text)).await.is_err()
        {
            break;
        }
    }
}

/// A `ws://` echo server for one connection.
async fn ws_echo() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    zeroclaw_spawn::spawn!(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        echo(stream).await;
    });
    port
}

/// A `wss://` echo server for one connection.
async fn wss_echo(acceptor: tokio_rustls::TlsAcceptor) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    zeroclaw_spawn::spawn!(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        if let Ok(stream) = acceptor.accept(stream).await {
            echo(stream).await;
        }
    });
    port
}

/// The operator's grant for this test instance: `hosts` in `egress_hosts`,
/// each allowed to resolve to loopback, and a `corp` profile trusting the
/// instance's `ca_pem` secret for `localhost` when `with_profile` is set.
fn egress(hosts: &[&str], with_profile: bool) -> EgressHostService {
    let hosts: Vec<String> = hosts.iter().map(|host| (*host).to_string()).collect();
    EgressHostService::new(EgressPolicyResolver::new(move |_| {
        let policy = EgressPolicy::new(&hosts, &hosts, &[], 8)?;
        if !with_profile {
            return Ok(policy);
        }
        policy.with_tls_profiles([TlsProfile::new(
            TlsProfileName::new("corp")?,
            &["localhost".to_string()],
            false,
            Some(SecretPropertyRef::parse("ca_pem".to_string()).expect("ca_pem reference")),
            None,
        )?])
    }))
}

struct Run<'a> {
    url: String,
    profile: Option<&'a str>,
    ca_pem: Option<&'a str>,
    grant_websocket: bool,
    egress: Option<EgressHostService>,
}

/// Instantiate the compiled fixture under one scope and run it once.
async fn run(run: Run<'_>) -> Result<String, String> {
    let manifest = manifest();
    let mut grants = vec![PluginPermission::ConfigRead];
    if run.grant_websocket {
        grants.push(PluginPermission::WebSocketClient);
    }
    // Unique per call so the process-wide connection budget never couples
    // concurrently running tests.
    let binding = format!("websocket-{}", run.url.replace(['/', ':', '.'], "-"));
    let scope =
        PluginInstanceScope::from_manifest(&manifest, PluginCapability::Tool, &binding, grants)
            .expect("admit fixture scope");
    let mut configured = HashMap::from([("url".to_string(), run.url.clone())]);
    if let Some(profile) = run.profile {
        configured.insert("profile".to_string(), profile.to_string());
    }
    if let Some(ca_pem) = run.ca_pem {
        configured.insert("ca_pem".to_string(), ca_pem.to_string());
    }
    let resolver_manifest = manifest.clone();
    let services = PluginHostServices::new(
        PluginConfigResolver::new(move |scope| {
            resolve_plugin_config(&resolver_manifest, scope, Some(&configured))
        }),
        state_service(),
    );
    let component = admit_fixture(&fixture(), &manifest);
    let mut plugin =
        runtime::create_plugin_with_egress(&component, &scope, &services, limits(), run.egress)
            .await
            .map_err(|error| format!("instantiate: {error:#}"))?;
    let result = runtime::call_execute(&mut plugin, b"{}")
        .await
        .map_err(|error| format!("{error:#}"))?;
    assert!(result.success, "fixture reports success on the Ok path");
    Ok(result.output.as_str().to_string())
}

fn ws(port: u16, egress: Option<EgressHostService>) -> Run<'static> {
    Run {
        url: format!("ws://localhost:{port}/echo"),
        profile: None,
        ca_pem: None,
        grant_websocket: true,
        egress,
    }
}

#[tokio::test]
async fn ws_reaches_a_granted_destination_without_a_plaintext_exception() {
    let port = ws_echo().await;
    let output = run(ws(port, Some(egress(&["localhost"], false)))).await;
    assert_eq!(output.as_deref(), Ok("ping"));
}

#[tokio::test]
async fn websockets_reach_nothing_outside_the_grant() {
    let port = ws_echo().await;
    let denied = run(ws(port, Some(egress(&["other.example.com"], false))))
        .await
        .expect_err("an ungranted destination must be refused");
    assert!(denied.contains("destination-denied"), "got: {denied}");

    let no_authority = run(ws(port, None))
        .await
        .expect_err("a store with no egress authority has no reach");
    assert!(
        no_authority.contains("destination-denied"),
        "got: {no_authority}"
    );
}

#[tokio::test]
async fn a_component_importing_websocket_cannot_load_without_the_grant() {
    let port = ws_echo().await;
    let refused = run(Run {
        grant_websocket: false,
        ..ws(port, Some(egress(&["localhost"], false)))
    })
    .await
    .expect_err("an ungranted scope must not link the websocket import");
    assert!(refused.starts_with("instantiate:"), "got: {refused}");
}

#[tokio::test]
async fn wss_trusts_a_custom_ca_only_through_the_selected_profile() {
    let pki = TestPki::new();

    let port = wss_echo(pki.acceptor()).await;
    let untrusted = run(Run {
        url: format!("wss://localhost:{port}/echo"),
        ..ws(port, Some(egress(&["localhost"], true)))
    })
    .await
    .expect_err("the test CA is not a system root");
    assert!(untrusted.contains("tls-failed"), "got: {untrusted}");

    let port = wss_echo(pki.acceptor()).await;
    let trusted = run(Run {
        url: format!("wss://localhost:{port}/echo"),
        profile: Some("corp"),
        ca_pem: Some(&pki.ca_pem),
        ..ws(port, Some(egress(&["localhost"], true)))
    })
    .await;
    assert_eq!(trusted.as_deref(), Ok("ping"));
}
