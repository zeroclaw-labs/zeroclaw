//! Real Component Model coverage for the typed plugin socket resource.
//!
//! A compiled tool component (`tests/fixtures/tool-socket-fixture`) opens host
//! sockets through the tool runtime exactly as a deployed plugin would, against
//! loopback TCP, TLS, and STARTTLS servers. Reach comes only from the egress
//! grant (ADR-014): the destination must be in `egress_hosts`, plaintext needs
//! no separate exception, and a TLS profile selects certificates without
//! granting anything.

#![cfg(feature = "plugins-wasm-cranelift")]

mod support;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, OnceLock};

use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
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
                .join("tests/fixtures/tool-socket-fixture");
            let target_dir =
                PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("tool-socket-plugin-fixture");
            let status = Command::new(env!("CARGO"))
                .current_dir(&fixture_dir)
                .args([
                    "build",
                    "--locked",
                    "--quiet",
                    "--package",
                    "zeroclaw-tool-socket-plugin-fixture",
                    "--target",
                    "wasm32-wasip2",
                    "--target-dir",
                ])
                .arg(&target_dir)
                .status()
                .expect("run Cargo for the tool socket component fixture");
            assert!(
                status.success(),
                "tool socket fixture must build; install the wasm32-wasip2 target"
            );
            let wasm =
                target_dir.join("wasm32-wasip2/debug/zeroclaw_tool_socket_plugin_fixture.wasm");
            assert!(wasm.is_file(), "tool socket fixture WASM was not produced");
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
        name: "tool-socket-fixture".to_string(),
        version: "0.0.0".to_string(),
        description: None,
        author: None,
        wasm_path: Some("tool-socket-fixture.wasm".to_string()),
        wasm_sha256: None,
        capabilities: vec![PluginCapability::Tool],
        permissions: vec![PluginPermission::ConfigRead, PluginPermission::SocketClient],
        config_schema: Some(serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "host": {"type": "string"},
                "port": {"type": "integer"},
                "mode": {"type": "string"},
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
            rcgen::CertificateParams::new(vec!["Socket E2E CA".to_string()]).expect("CA params");
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

/// Echo whatever arrives on one accepted plaintext connection.
async fn plain_echo() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    zeroclaw_spawn::spawn!(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut bytes = [0_u8; 1024];
        while let Ok(count) = stream.read(&mut bytes).await {
            if count == 0 || stream.write_all(&bytes[..count]).await.is_err() {
                break;
            }
        }
    });
    port
}

/// Echo over TLS from the first byte.
async fn tls_echo(acceptor: tokio_rustls::TlsAcceptor) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    zeroclaw_spawn::spawn!(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let Ok(mut stream) = acceptor.accept(stream).await else {
            return;
        };
        let mut bytes = [0_u8; 1024];
        while let Ok(count) = stream.read(&mut bytes).await {
            if count == 0 || stream.write_all(&bytes[..count]).await.is_err() {
                break;
            }
        }
    });
    port
}

/// Answer `STARTTLS\r\n` with `OK\r\n` in plaintext, then echo over TLS.
async fn starttls_echo(acceptor: tokio_rustls::TlsAcceptor) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    zeroclaw_spawn::spawn!(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut command = [0_u8; 10];
        if stream.read_exact(&mut command).await.is_err() || &command != b"STARTTLS\r\n" {
            return;
        }
        stream.write_all(b"OK\r\n").await.expect("reply OK");
        let Ok(mut stream) = acceptor.accept(stream).await else {
            return;
        };
        let mut bytes = [0_u8; 1024];
        while let Ok(count) = stream.read(&mut bytes).await {
            if count == 0 || stream.write_all(&bytes[..count]).await.is_err() {
                break;
            }
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
    port: u16,
    mode: &'a str,
    profile: Option<&'a str>,
    ca_pem: Option<&'a str>,
    grant_sockets: bool,
    egress: Option<EgressHostService>,
}

/// Instantiate the compiled fixture under one scope and run it once.
async fn run(run: Run<'_>) -> Result<String, String> {
    let manifest = manifest();
    let mut grants = vec![PluginPermission::ConfigRead];
    if run.grant_sockets {
        grants.push(PluginPermission::SocketClient);
    }
    // Unique per call so the process-wide connection budget never couples
    // concurrently running tests.
    let binding = format!("socket-{}-{}", run.mode, run.port);
    let scope =
        PluginInstanceScope::from_manifest(&manifest, PluginCapability::Tool, &binding, grants)
            .expect("admit fixture scope");
    let mut configured = HashMap::from([
        ("host".to_string(), "localhost".to_string()),
        ("port".to_string(), run.port.to_string()),
        ("mode".to_string(), run.mode.to_string()),
    ]);
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

fn plain(port: u16, egress: Option<EgressHostService>) -> Run<'static> {
    Run {
        port,
        mode: "plaintext",
        profile: None,
        ca_pem: None,
        grant_sockets: true,
        egress,
    }
}

#[tokio::test]
async fn plaintext_reaches_a_granted_destination_without_a_plaintext_exception() {
    let port = plain_echo().await;
    let output = run(plain(port, Some(egress(&["localhost"], false)))).await;
    assert_eq!(output.as_deref(), Ok("ping"));
}

#[tokio::test]
async fn sockets_reach_nothing_outside_the_grant() {
    let port = plain_echo().await;
    let denied = run(plain(port, Some(egress(&["other.example.com"], false))))
        .await
        .expect_err("an ungranted destination must be refused");
    assert!(denied.contains("access-denied"), "got: {denied}");

    let no_authority = run(plain(port, None))
        .await
        .expect_err("a store with no egress authority has no reach");
    assert!(
        no_authority.contains("access-denied"),
        "got: {no_authority}"
    );
}

#[tokio::test]
async fn a_component_importing_sockets_cannot_load_without_the_grant() {
    let port = plain_echo().await;
    let refused = run(Run {
        grant_sockets: false,
        ..plain(port, Some(egress(&["localhost"], false)))
    })
    .await
    .expect_err("an ungranted scope must not link the socket import");
    assert!(refused.starts_with("instantiate:"), "got: {refused}");
}

#[tokio::test]
async fn direct_tls_trusts_a_custom_ca_only_through_the_selected_profile() {
    let pki = TestPki::new();

    let port = tls_echo(pki.acceptor()).await;
    let untrusted = run(Run {
        mode: "tls",
        ..plain(port, Some(egress(&["localhost"], true)))
    })
    .await
    .expect_err("the test CA is not a system root");
    assert!(
        untrusted.contains("tls-handshake-failed"),
        "got: {untrusted}"
    );

    let port = tls_echo(pki.acceptor()).await;
    let trusted = run(Run {
        mode: "tls",
        profile: Some("corp"),
        ca_pem: Some(&pki.ca_pem),
        ..plain(port, Some(egress(&["localhost"], true)))
    })
    .await;
    assert_eq!(trusted.as_deref(), Ok("ping"));
}

#[tokio::test]
async fn a_profile_is_refused_on_plaintext_and_when_unknown() {
    let port = plain_echo().await;
    let on_plaintext = run(Run {
        profile: Some("corp"),
        ..plain(port, Some(egress(&["localhost"], true)))
    })
    .await
    .expect_err("a profile on plaintext is an invalid request");
    assert!(
        on_plaintext.contains("invalid-request"),
        "got: {on_plaintext}"
    );

    let port = tls_echo(TestPki::new().acceptor()).await;
    let unknown = run(Run {
        mode: "tls",
        profile: Some("absent"),
        ..plain(port, Some(egress(&["localhost"], true)))
    })
    .await
    .expect_err("an unknown profile is an invalid request");
    assert!(unknown.contains("invalid-request"), "got: {unknown}");
}

#[tokio::test]
async fn starttls_negotiates_in_plaintext_then_upgrades_in_place() {
    let pki = TestPki::new();
    let port = starttls_echo(pki.acceptor()).await;
    let upgraded = run(Run {
        mode: "starttls",
        profile: Some("corp"),
        ca_pem: Some(&pki.ca_pem),
        ..plain(port, Some(egress(&["localhost"], true)))
    })
    .await;
    assert_eq!(upgraded.as_deref(), Ok("ping"));
}
