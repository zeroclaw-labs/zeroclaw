#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn shell_quote(path: &Path) -> String {
    shell_quote_text(&path.display().to_string())
}

fn shell_quote_text(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn tcl_brace(path: &Path) -> String {
    let path = path.display().to_string();
    assert!(!path.contains('}'), "test path cannot be Tcl-brace quoted");
    format!("{{{path}}}")
}

fn daemon_fixture() -> (tempfile::TempDir, std::path::PathBuf, u16, String) {
    let config_dir = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    std::fs::write(
        config_dir.path().join("config.toml"),
        format!("[gateway]\nport = {port}\nrequire_pairing = false\n"),
    )
    .unwrap();
    let data_dir = config_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let obstruction = data_dir.join("daemon.sock");
    std::fs::create_dir(&obstruction).unwrap();
    let command = format!(
        "LC_ALL=C TERM=dumb {} --config-dir {} daemon --port {port} --allow-degraded-security",
        shell_quote(Path::new(env!("CARGO_BIN_EXE_zeroclaw"))),
        shell_quote(config_dir.path()),
    );
    (config_dir, obstruction, port, command)
}

fn endpoint_probe(socket: &Path, port: u16) -> String {
    let rpc_probe = r#"import socket,sys
s=socket.socket(socket.AF_UNIX)
s.settimeout(2)
s.connect(sys.argv[1])
s.sendall(b'{\"jsonrpc\":\"2.0\",\"method\":\"initialize\",\"params\":{\"protocol_version\":1},\"id\":1}\n')
sys.exit(0 if b'\"result\"' in s.recv(4096) else 1)"#;
    format!(
        "until curl -fsS --max-time 2 http://127.0.0.1:{port}/health >/dev/null; do sleep 0.05; done; until python3 -c {} {}; do sleep 0.05; done; echo ENDPOINTS_READY",
        shell_quote_text(rpc_probe),
        shell_quote(socket),
    )
}

fn http_status(port: u16, request: &[u8]) -> Option<u16> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .ok()?;
    stream.write_all(request).ok()?;

    let mut response = [0_u8; 512];
    let read = stream.read(&mut response).ok()?;
    let status_line = std::str::from_utf8(&response[..read])
        .ok()?
        .lines()
        .next()?;
    status_line.split_whitespace().nth(1)?.parse().ok()
}

#[test]
fn daemon_surfaces_retired_wati_config_without_leaking_tokens() {
    let cases = [
        (
            "current",
            r#"schema_version = 3

[gateway]
require_pairing = false

[channels.wati.exact_head_smoke]
enabled = true
api_token = "WATI_CURRENT_PLACEHOLDER_MUST_NOT_APPEAR"
api_url = "https://example.invalid"
allowed_numbers = ["1234567890"]
"#,
            "channels.wati",
            "WATI_CURRENT_PLACEHOLDER_MUST_NOT_APPEAR",
        ),
        (
            "legacy",
            r#"[gateway]
require_pairing = false

[channels_config.wati]
enabled = true
api_token = "WATI_LEGACY_PLACEHOLDER_MUST_NOT_APPEAR"
api_url = "https://example.invalid"
allowed_numbers = ["1234567890"]
"#,
            "channels_config.wati",
            "WATI_LEGACY_PLACEHOLDER_MUST_NOT_APPEAR",
        ),
    ];

    for (case, raw_config, expected_path, placeholder) in cases {
        let config_dir = tempfile::tempdir().unwrap();
        std::fs::write(config_dir.path().join("config.toml"), raw_config).unwrap();
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();

        let mut child = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
            .arg("--config-dir")
            .arg(config_dir.path())
            .arg("daemon")
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .arg("--allow-degraded-security")
            .env("LC_ALL", "C")
            .env("TERM", "dumb")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let health_request =
            b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut health_status = None;
        let mut early_exit = None;
        while Instant::now() < deadline {
            if let Some(status) = child.try_wait().unwrap() {
                early_exit = Some(status);
                break;
            }
            health_status = http_status(port, health_request);
            if health_status == Some(200) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        if early_exit.is_some() || health_status != Some(200) {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "{case} daemon failed before health readiness: exit={early_exit:?}, health={health_status:?}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }

        let body = r#"{"text":"hello","waId":"1234567890","fromMe":false}"#;
        let request = format!(
            "POST /wati HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let post_status = http_status(port, request.as_bytes());

        let _ = child.kill();
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert_eq!(health_status, Some(200), "{case}: daemon health");
        assert!(
            matches!(post_status, Some(404 | 405)),
            "{case}: retired POST /wati must be unavailable, got {post_status:?}"
        );
        assert!(
            stderr.contains("retired WATI channel config section")
                && stderr.contains(expected_path),
            "{case}: stderr must name retired path {expected_path}: {stderr}"
        );
        assert!(
            !stdout.contains(placeholder) && !stderr.contains(placeholder),
            "{case}: placeholder WATI token leaked to process output"
        );
    }
}

#[test]
fn daemon_feedback_follows_foreground_job_control_transitions() {
    let (_foreground_config, foreground_obstruction, foreground_port, foreground_command) =
        daemon_fixture();
    let (_background_config, background_obstruction, _background_port, background_command) =
        daemon_fixture();
    let foreground_probe = endpoint_probe(&foreground_obstruction, foreground_port);
    let script = format!(
        r#"
set timeout 15
log_user 1

spawn -noecho /bin/sh -i
expect -re {{[$#] $}}
send -- "stty -echo\r"
expect -re {{[$#] $}}
send -- {{trap 'for pid in $(jobs -p); do kill -TERM "$pid"; done; wait' EXIT\r}}
expect -re {{[$#] $}}
send -- "set -m\r"
expect -re {{[$#] $}}
send -- "{foreground_command}\r"
expect "daemon starting"
send -- "\032"
expect -re {{Stopped}}
send -- "bg\r"
expect -re {{[$#] $}}
file delete -force {foreground_obstruction}
send -- {{{foreground_probe}}}
send -- "\r"
expect "ENDPOINTS_READY"
if {{[string first "daemon ready" $expect_out(buffer)] >= 0}} {{
    puts stderr "background daemon printed its ready banner before endpoint readiness"
    exit 20
}}
after 200
set timeout 0
expect {{
    "daemon ready" {{
        puts stderr "background daemon printed its ready banner"
        exit 20
    }}
    timeout {{}}
}}
set timeout 15
send -- "kill -TERM %1\r"
expect -re {{[$#] $}}
send -- "wait %1\r"
expect -re {{[$#] $}}
send -- "exit\r"
expect eof

set timeout 15
spawn -noecho /bin/sh -i
expect -re {{[$#] $}}
send -- "stty -echo\r"
expect -re {{[$#] $}}
send -- {{trap 'for pid in $(jobs -p); do kill -TERM "$pid"; done; wait' EXIT\r}}
expect -re {{[$#] $}}
send -- "set -m\r"
expect -re {{[$#] $}}
send -- "{background_command} &\r"
expect -re {{\[[0-9]+\] [0-9]+}}
send -- "fg\r"
file delete -force {background_obstruction}
expect "daemon ready"
send -- "\003"
expect -re {{[$#] $}}
send -- "exit\r"
expect eof
"#,
        foreground_obstruction = tcl_brace(&foreground_obstruction),
        foreground_probe = foreground_probe,
        background_obstruction = tcl_brace(&background_obstruction),
    );

    let output = Command::new("expect")
        .args(["-c", &script])
        .output()
        .expect("expect must be installed for the PTY regression test");
    assert!(
        output.status.success(),
        "production-binary PTY transitions failed with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
