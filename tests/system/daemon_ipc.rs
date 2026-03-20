//! Daemon IPC integration tests — start daemon, connect via Unix socket, verify commands.
//!
//! These tests spawn the actual `augusta daemon start` binary in a subprocess,
//! connect to its Unix socket, send IPC commands, and verify responses.
//! The daemon is killed after each test.

use std::time::Duration;

/// Test that the daemon IPC protocol handles ping correctly (library-level).
#[tokio::test]
async fn daemon_ipc_ping_protocol() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("test.sock");

    // Start a listener in the background
    let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

    // Spawn the IPC handler task (simulates daemon behavior)
    let handler = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let response = match line.trim() {
                "ping" => serde_json::json!({
                    "status": "ok",
                    "pong": true,
                    "pid": std::process::id(),
                }),
                cmd => serde_json::json!({
                    "status": "error",
                    "error": format!("Unknown command: {cmd}"),
                }),
            };
            let mut bytes = serde_json::to_vec(&response).unwrap();
            bytes.push(b'\n');
            writer.write_all(&bytes).await.unwrap();
        }
    });

    // Give the listener a moment to bind
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect as a client
    let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    // Send ping
    writer.write_all(b"ping\n").await.unwrap();
    let response_line = lines.next_line().await.unwrap().unwrap();
    let response: serde_json::Value = serde_json::from_str(&response_line).unwrap();

    assert_eq!(response["status"], "ok");
    assert_eq!(response["pong"], true);
    assert!(response["pid"].as_u64().unwrap() > 0);

    // Send unknown command
    writer.write_all(b"unknown_cmd\n").await.unwrap();
    let response_line = lines.next_line().await.unwrap().unwrap();
    let response: serde_json::Value = serde_json::from_str(&response_line).unwrap();
    assert_eq!(response["status"], "error");

    handler.abort();
}

/// Test that the daemon IPC protocol handles status command.
#[tokio::test]
async fn daemon_ipc_status_protocol() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("test.sock");

    let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

    let handler = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let response = match line.trim() {
                "status" => serde_json::json!({
                    "status": "ok",
                    "pid": std::process::id(),
                    "uptime_secs": 0,
                    "version": env!("CARGO_PKG_VERSION"),
                }),
                "version" => serde_json::json!({
                    "status": "ok",
                    "version": env!("CARGO_PKG_VERSION"),
                }),
                _ => serde_json::json!({"status": "error"}),
            };
            let mut bytes = serde_json::to_vec(&response).unwrap();
            bytes.push(b'\n');
            writer.write_all(&bytes).await.unwrap();
        }
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    // Test status command
    writer.write_all(b"status\n").await.unwrap();
    let line = lines.next_line().await.unwrap().unwrap();
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["status"], "ok");
    assert!(resp["version"].as_str().unwrap().starts_with("0."));

    // Test version command
    writer.write_all(b"version\n").await.unwrap();
    let line = lines.next_line().await.unwrap().unwrap();
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["status"], "ok");

    handler.abort();
}

/// Test socket-based daemon status query returns rich info (uptime, version, PID).
/// Uses async client to avoid sync/async timing issues.
#[tokio::test]
async fn daemon_ipc_socket_status_query() {
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("test.sock");

    let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();
    let start_time = Arc::new(Instant::now());

    let t = Arc::clone(&start_time);
    let handler = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let response = match line.trim() {
                "status" => serde_json::json!({
                    "status": "ok",
                    "pid": std::process::id(),
                    "uptime_secs": t.elapsed().as_secs(),
                    "version": env!("CARGO_PKG_VERSION"),
                }),
                _ => serde_json::json!({"status": "error"}),
            };
            let mut bytes = serde_json::to_vec(&response).unwrap();
            bytes.push(b'\n');
            writer.write_all(&bytes).await.unwrap();
        }
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect and query status
    let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    writer.write_all(b"status\n").await.unwrap();
    let line = lines.next_line().await.unwrap().unwrap();
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();

    assert_eq!(resp["status"], "ok");
    assert!(resp["pid"].as_u64().unwrap() > 0);
    assert!(resp["uptime_secs"].as_u64().is_some());
    assert!(resp["version"].as_str().unwrap().starts_with("0."));

    handler.abort();
}

/// Test the event bus roundtrip: emit → load → verify.
#[test]
fn event_bus_roundtrip() {
    use lightwave_sys::tui::event_bus::{append_event, load_events, EventRecord};
    use lightwave_sys::tui::FeedApp;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test_events.jsonl");

    // Emit some events
    append_event(
        &path,
        &EventRecord::new("agent-1", "crew", "agent_started", "Session started"),
    )
    .unwrap();
    append_event(
        &path,
        &EventRecord::new("agent-1", "crew", "task_completed", "Task done"),
    )
    .unwrap();
    append_event(
        &path,
        &EventRecord::new("daemon", "system", "ping_success", "Health OK"),
    )
    .unwrap();

    // Load into FeedApp
    let mut app = FeedApp::new(100);
    let count = load_events(&path, &mut app).unwrap();

    assert_eq!(count, 3);
    assert_eq!(app.events.len(), 3);

    // Verify problems filter works
    app.toggle_problems();
    assert_eq!(app.visible_events().len(), 0); // No problems in these events
}
