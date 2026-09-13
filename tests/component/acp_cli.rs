use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use zeroclaw_infra::acp_session_store::AcpSessionStore;

#[test]
fn standalone_acp_agent_flag_selects_alias_less_session_owner() {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    let session_cwd = tempfile::tempdir().expect("temp session cwd");
    std::fs::write(
        config_dir.path().join("config.toml"),
        r#"schema_version = 3

[providers.models.ollama.default]
model = "test-model"

[risk_profiles.default]

[runtime_profiles.default]

[agents.fable]
model_provider = "ollama.default"
risk_profile = "default"
runtime_profile = "default"

[agents.other]
model_provider = "ollama.default"
risk_profile = "default"
runtime_profile = "default"
"#,
    )
    .expect("write ACP config");

    let mut child = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("ZEROCLAW_CONFIG_DIR", config_dir.path())
        .env("RUST_LOG", "off")
        .args(["acp", "--agent", "fable"])
        .current_dir(session_cwd.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start standalone ACP");

    let stdout = child.stdout.take().expect("ACP stdout");
    let (response_tx, response_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let line = line.expect("read ACP stdout");
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if response_tx.send(value).is_err() {
                return;
            }
        }
    });

    let initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize"
    });
    writeln!(child.stdin.as_mut().expect("ACP stdin"), "{}", initialize).expect("write initialize");
    child
        .stdin
        .as_mut()
        .expect("ACP stdin")
        .flush()
        .expect("flush initialize");
    let initialize_response = match response_rx.recv_timeout(Duration::from_secs(60)) {
        Ok(response) => response,
        Err(error) => {
            let status_before_kill = child.try_wait().expect("inspect failed ACP");
            let _ = child.kill();
            let output = child.wait_with_output().expect("collect failed ACP");
            panic!(
                "standalone ACP initialize failed ({error}); binary={}; status_before_kill={status_before_kill:?}; stderr:\n{}",
                env!("CARGO_BIN_EXE_zeroclaw"),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    };
    assert_eq!(initialize_response["id"], 1);
    assert_eq!(initialize_response["result"]["protocolVersion"], 1);

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "session/new",
        "params": {
            "cwd": session_cwd.path().to_string_lossy(),
            "mcpServers": []
        }
    });
    writeln!(child.stdin.as_mut().expect("ACP stdin"), "{}", request).expect("write session/new");
    child
        .stdin
        .as_mut()
        .expect("ACP stdin")
        .flush()
        .expect("flush session/new");

    let response = match response_rx.recv_timeout(Duration::from_secs(20)) {
        Ok(response) => response,
        Err(error) => {
            let _ = child.kill();
            let output = child.wait_with_output().expect("collect failed ACP");
            panic!(
                "ACP session/new timed out ({error})\nstderr:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    };

    let session_id = response
        .get("result")
        .and_then(|result| result.get("sessionId"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("session/new failed: {response}"));

    drop(child.stdin.take());
    let status = child.wait().expect("wait for ACP shutdown");
    reader.join().expect("join ACP stdout reader");
    assert!(
        status.success(),
        "ACP should exit cleanly after stdin closes"
    );

    let store =
        AcpSessionStore::new(&config_dir.path().join("data")).expect("open persisted ACP sessions");
    let session = store
        .load_session(session_id)
        .expect("load persisted session")
        .expect("session should be persisted");
    assert_eq!(session.agent_alias, "fable");
}
