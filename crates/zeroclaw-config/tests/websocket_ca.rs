#![cfg(feature = "ws-transport")]

use std::{process::Command, sync::Arc, time::Duration};

use rcgen::{CertificateParams, KeyPair};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::TlsAcceptor;
use zeroclaw_config::schema::ws_connect_with_proxy;

// Each client has its own process environment; parallel tests never mutate
// SSL_CERT_FILE or the process-wide proxy policy in the parent test harness.
#[test]
fn websocket_tls_client() {
    let Ok(url) = std::env::var("ZEROCLAW_TEST_WS_URL") else {
        return;
    };
    let _ = rustls::crypto::ring::default_provider().install_default();
    let proxy = std::env::var("ZEROCLAW_TEST_WS_PROXY").ok();
    let expected_error = std::env::var("ZEROCLAW_TEST_WS_ERROR").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            ws_connect_with_proxy(&url, "channel.discord", proxy.as_deref()),
        )
        .await
        .expect("WebSocket connection timed out");
        if expected_error.is_empty() {
            let (_, response) = result.expect("WebSocket upgrade should succeed");
            assert_eq!(response.status(), 101);
        } else {
            let error = result.err().expect("Connection must be rejected");
            assert!(
                format!("{error:#}").contains(&expected_error),
                "expected {expected_error:?}, got {error:#}"
            );
        }
    });
}

fn check_connection(proxy: bool, scenario: &str) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let directory = tempfile::tempdir().unwrap();
        let bundle = directory.path().join("ca.pem");
        let mut params = CertificateParams::new(vec![
            if scenario == "wrong-host" {
                "host.invalid"
            } else {
                "127.0.0.1"
            }
            .into(),
        ])
        .unwrap();
        if scenario == "expired" {
            params.not_before = rcgen::date_time_ymd(1999, 1, 1);
            params.not_after = rcgen::date_time_ymd(2000, 1, 1);
        }
        let key = KeyPair::generate().unwrap();
        let certificate = params.self_signed(&key).unwrap();
        let pem = certificate.pem();
        let contents = match scenario {
            "empty-bundle" => "",
            "invalid-pem" => "-----BEGIN CERTIFICATE-----\n!\n-----END CERTIFICATE-----\n",
            "invalid-der" => "-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n",
            _ => &pem,
        };
        if scenario != "missing" {
            std::fs::write(&bundle, contents).unwrap();
        }
        let server_config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.der().clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )
        .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(server_config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let plain = scenario == "plain";
        let server = zeroclaw_spawn::spawn!(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            if proxy {
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    assert!(request.len() < 4096);
                    request.push(stream.read_u8().await.unwrap());
                }
                assert!(
                    String::from_utf8(request)
                        .unwrap()
                        .starts_with(&format!("CONNECT {address} HTTP/1.1\r\n"))
                );
                stream
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await
                    .unwrap();
            }
            if plain {
                tokio_tungstenite::accept_async(stream)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            } else {
                match acceptor.accept(stream).await {
                    Ok(tls) => tokio_tungstenite::accept_async(tls)
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string()),
                    Err(error) => Err(error.to_string()),
                }
            }
        });
        let expected_error = match scenario {
            "trusted" | "plain" => "",
            "unset" | "empty-env" => "UnknownIssuer",
            "wrong-host" => "certificate not valid for name",
            "expired" => "expired",
            "missing" => "Failed to read WebSocket CA bundle from SSL_CERT_FILE",
            "empty-bundle" => "WebSocket CA bundle contains no certificates",
            "invalid-pem" => "Invalid PEM certificate in WebSocket CA bundle",
            "invalid-der" => "Invalid trust anchor in WebSocket CA bundle",
            _ => unreachable!(),
        };
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "websocket_tls_client", "--nocapture"])
            .env(
                "ZEROCLAW_TEST_WS_URL",
                format!("{}://{address}/", if plain { "ws" } else { "wss" }),
            )
            .env("ZEROCLAW_TEST_WS_ERROR", expected_error)
            .env_remove("ZEROCLAW_TEST_WS_PROXY")
            .env_remove("SSL_CERT_FILE");
        if proxy {
            child.env("ZEROCLAW_TEST_WS_PROXY", format!("http://{address}"));
        }
        if scenario != "unset" {
            child.env(
                "SSL_CERT_FILE",
                if scenario == "empty-env" {
                    std::ffi::OsStr::new("")
                } else if plain {
                    directory.path().as_os_str()
                } else {
                    bundle.as_os_str()
                },
            );
        }
        let output = tokio::task::spawn_blocking(move || child.output().unwrap())
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "proxy={proxy}, scenario={scenario}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if expected_error.is_empty() {
            tokio::time::timeout(Duration::from_secs(10), server)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        } else {
            server.abort();
        }
    });
}

#[test]
fn direct_websocket_ca_policy() {
    for scenario in [
        "trusted",
        "unset",
        "empty-env",
        "wrong-host",
        "expired",
        "missing",
        "empty-bundle",
        "invalid-pem",
        "invalid-der",
        "plain",
    ] {
        check_connection(false, scenario);
    }
}

#[test]
fn proxied_websocket_ca_policy() {
    for scenario in [
        "trusted",
        "unset",
        "empty-env",
        "wrong-host",
        "expired",
        "missing",
        "empty-bundle",
        "invalid-pem",
        "invalid-der",
        "plain",
    ] {
        check_connection(true, scenario);
    }
}
