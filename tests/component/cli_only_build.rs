//! A build without the gateway (the CLI-only build) points users to the
//! standard build for the dashboard instead of failing with a build hint.

use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};

const NO_DASHBOARD: &str = "This ZeroClaw build has no dashboard or web gateway";

fn zeroclaw(config_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .arg("--config-dir")
        .arg(config_dir)
        .args(args)
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .env("ZEROCLAW_SOCKET", config_dir.join("daemon.sock"))
        .output()
        .unwrap()
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn gateway_and_desktop_commands_point_to_the_standard_build() {
    let config_dir = tempfile::tempdir().unwrap();
    for args in [
        &["gateway"][..],
        &["gateway", "start"],
        &["gateway", "restart"],
        &["gateway", "get-paircode"],
        &["desktop"],
    ] {
        let output = zeroclaw(config_dir.path(), args);
        let text = combined(&output);
        assert!(
            !output.status.success(),
            "{args:?} must fail in a build without the gateway: {text}"
        );
        assert!(
            text.contains(NO_DASHBOARD) && text.contains("install.sh"),
            "{args:?} must point to the standard build: {text}"
        );
    }
}

#[test]
fn status_says_the_dashboard_is_not_in_this_build() {
    let config_dir = tempfile::tempdir().unwrap();
    let output = zeroclaw(config_dir.path(), &["status"]);
    let text = combined(&output);
    assert!(output.status.success(), "status failed: {text}");
    assert!(text.contains("not included in this build"), "{text}");
    assert!(!text.contains("MISSING"), "{text}");
}

#[test]
fn sop_admin_and_exit_code_status_do_not_contact_the_gateway_port() {
    let config_dir = tempfile::tempdir().unwrap();
    let gateway = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    gateway.set_nonblocking(true).unwrap();
    let port = gateway.local_addr().unwrap().port();
    std::fs::write(
        config_dir.path().join("config.toml"),
        format!("[gateway]\nhost = '127.0.0.1'\nport = {port}\n"),
    )
    .unwrap();

    for args in [
        &["sop", "pending"][..],
        &["sop", "logs", "run"],
        &["sop", "approve", "run"],
        &["sop", "deny", "run"],
    ] {
        let output = zeroclaw(config_dir.path(), args);
        assert!(!output.status.success(), "{args:?} should be unavailable");
        let text = combined(&output);
        assert!(
            text.contains(NO_DASHBOARD) && text.contains("install.sh"),
            "{text}"
        );
    }

    let status = zeroclaw(config_dir.path(), &["status", "--format", "exit-code"]);
    assert_eq!(status.status.code(), Some(1), "{}", combined(&status));
    assert_eq!(
        gateway.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "the CLI-only commands must leave the configured gateway untouched",
    );
}

#[cfg(unix)]
#[tokio::test]
async fn status_exit_code_reads_the_local_daemons_health() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    use tokio::time::{Duration, timeout};

    for (daemon_status, expected_exit) in [("ok", 0), ("error", 1)] {
        let dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(dir.path().join("daemon.sock")).unwrap();
        let config_dir = dir.path().to_owned();
        let command = tokio::task::spawn_blocking(move || {
            zeroclaw(&config_dir, &["status", "--format", "exit-code"])
        });

        timeout(Duration::from_secs(10), async {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            for (method, response) in [
                (
                    "initialize",
                    serde_json::json!({"server_version": env!("CARGO_PKG_VERSION")}),
                ),
                (
                    "health",
                    serde_json::json!({"components": {"daemon": {"status": daemon_status}}}),
                ),
            ] {
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], method);
                let reply =
                    serde_json::json!({"jsonrpc": "2.0", "id": request["id"], "result": response});
                stream
                    .get_mut()
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .unwrap();
            }
        })
        .await
        .expect("CLI should request health from the configured daemon socket");

        let output = command.await.unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected_exit),
            "{}",
            combined(&output)
        );
    }
}
