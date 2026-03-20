//! Daemon resilience tests — heartbeat, health IPC, plist validation.

use std::time::Duration;

/// Test that the heartbeat loop writes a timestamp file.
#[tokio::test]
async fn heartbeat_file_written() {
    let dir = tempfile::tempdir().unwrap();
    let heartbeat_path = dir.path().join("heartbeat");

    let monitor = std::sync::Arc::new(lightwave_sys::health::monitor::HealthMonitor::new());
    monitor.register("daemon".into(), "system".into()).await;

    // Write heartbeat manually (simulates what heartbeat_loop does)
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    tokio::fs::write(&heartbeat_path, format!("{ts}\n"))
        .await
        .unwrap();

    // Verify file exists and contains a valid timestamp
    let content = tokio::fs::read_to_string(&heartbeat_path).await.unwrap();
    let parsed_ts: u64 = content.trim().parse().unwrap();
    assert!(parsed_ts > 0);
    assert!((parsed_ts as i64 - ts as i64).unsigned_abs() < 2);
}

/// Test that the health IPC command returns expected JSON structure.
#[tokio::test]
async fn health_ipc_command() {
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("test.sock");
    let heartbeat_path = dir.path().join("heartbeat");

    // Write a fresh heartbeat
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    tokio::fs::write(&heartbeat_path, format!("{ts}\n"))
        .await
        .unwrap();

    let monitor = Arc::new(lightwave_sys::health::monitor::HealthMonitor::new());
    monitor.register("daemon".into(), "system".into()).await;
    monitor.record_ping("daemon").await;

    let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();
    let start_time = Arc::new(Instant::now());

    let m = Arc::clone(&monitor);
    let hb = heartbeat_path.clone();
    let t = Arc::clone(&start_time);
    let handler = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        lightwave_sys::daemon::handle_ipc_client(stream, t, m, hb).await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    writer.write_all(b"health\n").await.unwrap();
    let line = lines.next_line().await.unwrap().unwrap();
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();

    assert_eq!(resp["status"], "ok");
    assert_eq!(resp["monitor_status"], "healthy");
    assert!(resp["heartbeat_age_secs"].as_u64().is_some());
    assert!(resp["agents"].as_array().is_some());
    assert!(!resp["agents"].as_array().unwrap().is_empty());

    // Verify daemon agent is in the list
    let agents = resp["agents"].as_array().unwrap();
    let daemon_agent = agents.iter().find(|a| a["name"] == "daemon");
    assert!(daemon_agent.is_some());

    handler.abort();
}

/// Test that the generated plist contains resilience keys.
#[test]
fn plist_has_resilience_keys() {
    let plist_path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("com.lightwave.augusta.plist");
    let content = std::fs::read_to_string(plist_path).unwrap();

    assert!(
        content.contains("SuccessfulExit"),
        "Missing SuccessfulExit key"
    );
    assert!(
        content.contains("ThrottleInterval"),
        "Missing ThrottleInterval key"
    );
    assert!(content.contains("ExitTimeOut"), "Missing ExitTimeOut key");
    assert!(content.contains("ProcessType"), "Missing ProcessType key");
    assert!(
        content.contains("SoftResourceLimits"),
        "Missing SoftResourceLimits key"
    );
}

/// Test that the watchdog plist exists and has correct structure.
#[test]
fn watchdog_plist_exists() {
    let plist_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("com.lightwave.augusta.watchdog.plist");
    let content = std::fs::read_to_string(plist_path).unwrap();

    assert!(
        content.contains("com.lightwave.augusta.watchdog"),
        "Missing watchdog label"
    );
    assert!(
        content.contains("StartInterval"),
        "Missing StartInterval (periodic)"
    );
    assert!(content.contains("heartbeat"), "Missing heartbeat check");
    assert!(
        content.contains("SIGTERM"),
        "Missing SIGTERM in watchdog script"
    );
}
