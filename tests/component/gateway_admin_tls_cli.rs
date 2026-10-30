//! Gateway restart must stop at local TLS refusal, before starting another
//! gateway; the subprocess also proves Fluent lookup at the CLI boundary.

use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn write_config(dir: &Path, port: u16, locale: &str, tls: &str) {
    std::fs::write(
        dir.join("config.toml"),
        format!(
            r#"schema_version = 3
locale = "{locale}"

[providers.models.ollama.default]
model = "test-model"
[risk_profiles.default]
[runtime_profiles.default]
[agents.default]
model_provider = "ollama.default"
risk_profile = "default"
runtime_profile = "default"

[gateway]
host = "127.0.0.1"
port = {port}
path_prefix = "/gw"
{tls}
"#
        ),
    )
    .unwrap();
}

async fn restart(dir: &Path, proxy: Option<&str>) -> Output {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_zeroclaw"));
    command
        .env_clear()
        .env("ZEROCLAW_CONFIG_DIR", dir)
        .env("ZEROCLAW_DATA_DIR", dir.join("data"))
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .args(["gateway", "restart"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(proxy) = proxy {
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            command.env(name, proxy);
        }
        command.env("NO_PROXY", "").env("no_proxy", "");
    }
    tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .expect("restart must return instead of launching another gateway")
        .expect("run CLI")
}

fn assert_refused(output: &Output, expected: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(stderr.contains(expected), "stdout={stdout} stderr={stderr}");
    let all = format!("{stdout}{stderr}");
    assert!(!all.contains("No existing gateway"), "{all}");
    assert!(!all.contains("Starting ZeroClaw Gateway"), "{all}");
    println!("refusal: {stderr}");
}

#[tokio::test]
async fn gateway_admin_restart_stops_on_local_tls_refusals() {
    for (cert, client_auth, expected) in [
        ("unused.pem", true, "requires a client certificate"),
        ("", false, "cert_path is empty"),
        (
            "missing.pem",
            false,
            "Failed to read gateway TLS certificate",
        ),
        (
            "invalid.pem",
            false,
            "Invalid PEM in gateway TLS certificate",
        ),
        ("empty.pem", false, "contained no certificates"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(
            dir.path().join("invalid.pem"),
            "-----BEGIN CERTIFICATE-----\ninvalid!\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("empty.pem"), "").unwrap();
        let path = if cert.is_empty() {
            String::new()
        } else {
            dir.path().join(cert).to_string_lossy().into_owned()
        };
        let tls = format!(
            "[gateway.tls]\nenabled = true\ncert_path = {:?}\nkey_path = \"unused.key\"\n[ gateway.tls.client_auth ]\nenabled = {client_auth}\nrequire_client_cert = true\n",
            path,
        );
        write_config(dir.path(), port, "en", &tls);
        assert_refused(&restart(dir.path(), None).await, expected);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "local TLS refusal must precede any network request"
        );
    }
}

#[tokio::test]
async fn gateway_admin_restart_renders_the_selected_fluent_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let catalog_dir = dir.path().join("data/ftl/ja");
    std::fs::create_dir_all(&catalog_dir).unwrap();
    std::fs::write(
        catalog_dir.join("cli.ftl"),
        "cli-gateway-admin-client-cert-required = TLS_CLIENT_CERT_CATALOG_FIXTURE\n",
    )
    .unwrap();
    write_config(
        dir.path(),
        0,
        "ja",
        "[gateway.tls]\nenabled = true\ncert_path = \"unused.pem\"\nkey_path = \"unused.key\"\n[gateway.tls.client_auth]\nenabled = true\nrequire_client_cert = true\n",
    );
    assert_refused(
        &restart(dir.path(), None).await,
        "TLS_CLIENT_CERT_CATALOG_FIXTURE",
    );
}

#[tokio::test]
async fn gateway_admin_restart_contacts_gateway_despite_proxy_environment() {
    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    write_config(dir.path(), port, "en", "");
    let server = ::zeroclaw_spawn::spawn!(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 1024];
        let len = stream.read(&mut request).await.unwrap();
        assert!(
            String::from_utf8_lossy(&request[..len])
                .starts_with("POST /gw/admin/shutdown HTTP/1.1\r\n")
        );
        stream
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
    });
    assert_refused(
        &restart(dir.path(), Some(&proxy_url)).await,
        "403 Forbidden",
    );
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), proxy.accept())
            .await
            .is_err(),
        "local admin traffic must not reach a proxy"
    );
}
