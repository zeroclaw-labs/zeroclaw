use anyhow::Result;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use zeroclaw::cp;
use zeroclaw::db::Registry;
use zeroclaw::lifecycle;

/// Helper: create a registry with a registered instance in a temp dir.
/// Returns (TempDir, db_path, instance_id, instance_dir).
fn setup_instance(name: &str, port: u16) -> (TempDir, PathBuf, String, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let cp_dir = tmp.path().join("cp");
    let instances_dir = cp_dir.join("instances");
    fs::create_dir_all(&instances_dir).unwrap();

    let db_path = cp_dir.join("registry.db");
    let registry = Registry::open(&db_path).unwrap();

    let id = uuid::Uuid::new_v4().to_string();
    let inst_dir = instances_dir.join(&id);
    fs::create_dir_all(&inst_dir).unwrap();

    // Create a minimal config.toml
    let config_path = inst_dir.join("config.toml");
    fs::write(
        &config_path,
        format!(
            r#"default_temperature = 0.7

[gateway]
port = {port}
host = "127.0.0.1"
require_pairing = true
"#
        ),
    )
    .unwrap();

    registry
        .create_instance(
            &id,
            name,
            port,
            config_path.to_str().unwrap(),
            None,
            None,
        )
        .unwrap();

    (tmp, db_path, id, inst_dir)
}

/// Helper: start an in-process axum server on a random port.
/// Returns the base URL and a shutdown sender.
async fn start_test_server(
    db_path: PathBuf,
) -> (String, tokio::sync::watch::Sender<bool>) {
    let state = cp::server::CpState {
        db_path: Arc::new(db_path),
    };
    let app = cp::server::build_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://127.0.0.1:{}", addr.port());

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.changed().await;
            })
            .await
            .unwrap();
    });

    (base_url, shutdown_tx)
}

// ── Gate 1: Crash detection -- DB=running + dead PID -> supervisor corrects ──

#[test]
fn gate1_crash_detection() -> Result<()> {
    let (_tmp, db_path, id, inst_dir) = setup_instance("crash-test", 18901);

    // Set DB status to "running" and write a pidfile with a dead PID
    let registry = Registry::open(&db_path)?;
    registry.update_status(&id, "running")?;
    lifecycle::write_pid(&inst_dir, 4_294_967)?; // extremely unlikely to be alive

    // Verify precondition
    assert_eq!(registry.get_instance(&id)?.unwrap().status, "running");
    assert!(inst_dir.join("daemon.pid").exists());

    // Run supervisor check
    cp::supervisor::check_all_instances(&db_path);

    // Verify: status corrected to stopped, pidfile removed
    let registry = Registry::open(&db_path)?;
    let inst = registry.get_instance(&id)?.unwrap();
    assert_eq!(inst.status, "stopped", "Status should be corrected to stopped");
    assert!(inst.pid.is_none(), "DB PID cache should be cleared");
    assert!(
        !inst_dir.join("daemon.pid").exists(),
        "Stale pidfile should be removed"
    );

    Ok(())
}

// ── Gate 2: Startup reconcile corrects DB=running + dead PID ──

#[test]
fn gate2_startup_reconcile_corrects_stale() -> Result<()> {
    let (_tmp, db_path, id, inst_dir) = setup_instance("reconcile-test", 18902);

    let registry = Registry::open(&db_path)?;
    registry.update_status(&id, "running")?;
    lifecycle::write_pid(&inst_dir, 4_294_967)?;
    drop(registry);

    // Run startup reconcile
    cp::supervisor::startup_reconcile(&db_path);

    let registry = Registry::open(&db_path)?;
    let inst = registry.get_instance(&id)?.unwrap();
    assert_eq!(inst.status, "stopped");
    assert!(!inst_dir.join("daemon.pid").exists());

    Ok(())
}

// ── Gate 2b: Drift detection -- DB=stopped but process alive and owned ──

#[test]
fn gate2b_drift_detection_stopped_but_alive() -> Result<()> {
    let (_tmp, db_path, id, inst_dir) = setup_instance("drift-test", 18903);

    // Spawn a real process with ZEROCLAW_HOME pointing to inst_dir.
    // This makes verify_pid_ownership return true, so live_status returns "running".
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .env("ZEROCLAW_HOME", inst_dir.to_str().unwrap())
        .spawn()?;
    let child_pid = child.id();

    // Write pidfile for the child
    lifecycle::write_pid(&inst_dir, child_pid)?;

    // DB says "stopped" (default from setup_instance)
    let registry = Registry::open(&db_path)?;
    assert_eq!(registry.get_instance(&id)?.unwrap().status, "stopped");
    drop(registry);

    // Run startup reconcile -- should detect alive process and correct DB to "running"
    cp::supervisor::startup_reconcile(&db_path);

    let registry = Registry::open(&db_path)?;
    let inst = registry.get_instance(&id)?.unwrap();
    assert_eq!(
        inst.status, "running",
        "Supervisor should correct DB from stopped to running when process is alive+owned"
    );
    assert_eq!(inst.pid, Some(child_pid), "DB PID cache should be set");
    drop(registry);

    // Cleanup: kill the child and reap via the Child handle to avoid zombies
    child.kill()?;
    let _ = child.wait();

    Ok(())
}

// ── Gate 3a: Health endpoint ──

#[tokio::test]
async fn gate3a_health_endpoint() -> Result<()> {
    let (_tmp, db_path, _id, _inst_dir) = setup_instance("health-test", 18904);

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client.get(format!("{base_url}/api/health")).send().await?;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await?;
    assert_eq!(body["status"], "ok");
    assert!(body["instances"].is_object());
    assert!(body["instances"]["health-test"].is_object());

    let _ = shutdown.send(true);
    Ok(())
}

// ── Gate 3b: List instances ──

#[tokio::test]
async fn gate3b_list_instances() -> Result<()> {
    let tmp = TempDir::new()?;
    let cp_dir = tmp.path().join("cp");
    fs::create_dir_all(cp_dir.join("instances"))?;
    let db_path = cp_dir.join("registry.db");
    let _registry = Registry::open(&db_path)?; // just create empty DB

    let (base_url, shutdown) = start_test_server(db_path.clone()).await;
    let client = reqwest::Client::new();

    // Empty DB -> empty array
    let resp = client
        .get(format!("{base_url}/api/instances"))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await?;
    assert!(body.as_array().unwrap().is_empty());

    // Register one, then list again
    let registry = Registry::open(&db_path)?;
    let inst_dir = cp_dir.join("instances").join("inst-1");
    fs::create_dir_all(&inst_dir)?;
    let config_path = inst_dir.join("config.toml");
    fs::write(&config_path, "default_temperature = 0.7\n")?;
    registry.create_instance(
        "inst-1",
        "my-agent",
        18910,
        config_path.to_str().unwrap(),
        None,
        None,
    )?;
    drop(registry);

    let resp = client
        .get(format!("{base_url}/api/instances"))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await?;
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "my-agent");

    let _ = shutdown.send(true);
    Ok(())
}

// ── Gate 3c: Get unknown instance -> 404 ──

#[tokio::test]
async fn gate3c_get_unknown_instance_404() -> Result<()> {
    let tmp = TempDir::new()?;
    let cp_dir = tmp.path().join("cp");
    fs::create_dir_all(cp_dir.join("instances"))?;
    let db_path = cp_dir.join("registry.db");
    let _registry = Registry::open(&db_path)?;

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base_url}/api/instances/ghost"))
        .send()
        .await?;
    assert_eq!(resp.status(), 404);

    let body: serde_json::Value = resp.json().await?;
    assert!(body["error"].as_str().unwrap().contains("ghost"));

    let _ = shutdown.send(true);
    Ok(())
}

// ── Gate 3d: Start unknown instance -> 404 ──

#[tokio::test]
async fn gate3d_start_unknown_404() -> Result<()> {
    let tmp = TempDir::new()?;
    let cp_dir = tmp.path().join("cp");
    fs::create_dir_all(cp_dir.join("instances"))?;
    let db_path = cp_dir.join("registry.db");
    let _registry = Registry::open(&db_path)?;

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base_url}/api/instances/ghost/start"))
        .send()
        .await?;
    assert_eq!(resp.status(), 404, "Start on unknown instance should be 404, not 500");

    let _ = shutdown.send(true);
    Ok(())
}

// ── Gate 3e: Stop unknown instance -> 404 ──

#[tokio::test]
async fn gate3e_stop_unknown_404() -> Result<()> {
    let tmp = TempDir::new()?;
    let cp_dir = tmp.path().join("cp");
    fs::create_dir_all(cp_dir.join("instances"))?;
    let db_path = cp_dir.join("registry.db");
    let _registry = Registry::open(&db_path)?;

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base_url}/api/instances/ghost/stop"))
        .send()
        .await?;
    assert_eq!(resp.status(), 404, "Stop on unknown instance should be 404");

    let _ = shutdown.send(true);
    Ok(())
}

// ── Gate 3f: Logs endpoint returns correct number of lines ──

#[tokio::test]
async fn gate3f_logs_returns_requested_lines() -> Result<()> {
    let (_tmp, db_path, _id, inst_dir) = setup_instance("log-test", 18905);

    // Write a log file with 10 lines
    let log_dir = inst_dir.join("logs");
    fs::create_dir_all(&log_dir)?;
    let content: String = (1..=10).map(|i| format!("line {i}\n")).collect();
    fs::write(log_dir.join("daemon.log"), content)?;

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base_url}/api/instances/log-test/logs?lines=3"))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await?;
    let lines = body["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 3, "Should return exactly 3 lines");
    assert_eq!(lines[0], "line 8");
    assert_eq!(lines[2], "line 10");

    let _ = shutdown.send(true);
    Ok(())
}

// ── Gate 3g: Logs cap at 10,000 ──

#[tokio::test]
async fn gate3g_logs_cap() -> Result<()> {
    let (_tmp, db_path, _id, inst_dir) = setup_instance("logcap-test", 18906);

    // Write a log file with 20 lines (we just check the param gets clamped)
    let log_dir = inst_dir.join("logs");
    fs::create_dir_all(&log_dir)?;
    let content: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    fs::write(log_dir.join("daemon.log"), content)?;

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    // Request 99999 lines - should be clamped, returning all 20
    let resp = client
        .get(format!("{base_url}/api/instances/logcap-test/logs?lines=99999"))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await?;
    let lines = body["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 20, "All 20 lines returned (file smaller than cap)");

    let _ = shutdown.send(true);
    Ok(())
}

// ── Gate 5: Supervisor skips locked instance ──

#[test]
fn gate5_supervisor_skips_locked_instance() -> Result<()> {
    let (_tmp, db_path, id, inst_dir) = setup_instance("locked-test", 18908);

    let registry = Registry::open(&db_path)?;
    registry.update_status(&id, "running")?;
    lifecycle::write_pid(&inst_dir, 4_294_967)?; // dead PID
    drop(registry);

    // Hold the lifecycle lock
    let _lock = lifecycle::acquire_lifecycle_lock(&inst_dir)?;

    // Run supervisor -- should skip this instance because lock is held
    cp::supervisor::check_all_instances(&db_path);

    // Status should remain "running" (not corrected) because supervisor skipped it
    let registry = Registry::open(&db_path)?;
    let inst = registry.get_instance(&id)?.unwrap();
    assert_eq!(
        inst.status, "running",
        "Supervisor should skip locked instance, leaving status unchanged"
    );

    Ok(())
}

// ── Gate 6: LifecycleError variants map to correct HTTP codes ──

#[tokio::test]
async fn gate6_lifecycle_error_http_codes() -> Result<()> {
    let (_tmp, db_path, _id, _inst_dir) = setup_instance("error-test", 18909);

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    // NotFound -> 404
    let resp = client
        .post(format!("{base_url}/api/instances/nonexistent/start"))
        .send()
        .await?;
    assert_eq!(resp.status(), 404);

    // NotRunning -> 409 (stop on a stopped instance)
    let resp = client
        .post(format!("{base_url}/api/instances/error-test/stop"))
        .send()
        .await?;
    assert_eq!(resp.status(), 409, "Stopping a stopped instance should be 409");

    let _ = shutdown.send(true);
    Ok(())
}

// ── Gate: DB PID column migration works ──

#[test]
fn gate_pid_column_migration() -> Result<()> {
    let tmp = TempDir::new()?;
    let db_path = tmp.path().join("test.db");

    // Create a DB with the old schema (no pid column)
    let conn = rusqlite::Connection::open(&db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    conn.execute_batch(
        "CREATE TABLE instances (
            id TEXT PRIMARY KEY NOT NULL,
            name TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'stopped',
            port INTEGER NOT NULL,
            config_path TEXT NOT NULL,
            workspace_dir TEXT,
            archived_at TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            migration_run_id TEXT
        );",
    )?;
    conn.execute(
        "INSERT INTO instances (id, name, status, port, config_path)
         VALUES ('old-1', 'legacy', 'running', 18801, '/old/config.toml')",
        [],
    )?;
    drop(conn);

    // Open via Registry which should run migration
    let registry = Registry::open(&db_path)?;
    let inst = registry.get_instance("old-1")?.unwrap();
    assert_eq!(inst.name, "legacy");
    assert!(inst.pid.is_none(), "Migrated row should have NULL pid");

    // update_pid should work on migrated DB
    registry.update_pid("old-1", Some(42))?;
    let inst = registry.get_instance("old-1")?.unwrap();
    assert_eq!(inst.pid, Some(42));

    Ok(())
}

// ── Gate 3h: Full lifecycle via HTTP (requires binary) ──

#[tokio::test]
#[ignore = "requires ZEROCLAW_BIN to be set"]
async fn gate3h_full_lifecycle_via_http() -> Result<()> {
    let (_tmp, db_path, _id, _inst_dir) = setup_instance("lifecycle-http", 18920);

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    // Start
    let resp = client
        .post(format!("{base_url}/api/instances/lifecycle-http/start"))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);

    // Status check
    let resp = client
        .get(format!("{base_url}/api/instances/lifecycle-http"))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await?;
    assert_eq!(body["status"], "running");

    // Stop
    let resp = client
        .post(format!("{base_url}/api/instances/lifecycle-http/stop"))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);

    let _ = shutdown.send(true);
    Ok(())
}

// ── Gate 4: Graceful shutdown (requires binary) ──

#[tokio::test]
#[ignore = "requires built binary"]
async fn gate4_graceful_shutdown() -> Result<()> {
    use std::process::Command;

    let child = Command::new(env!("CARGO_BIN_EXE_zeroclaw-cp"))
        .arg("serve")
        .env("ZEROCLAW_CP_PORT", "18930")
        .spawn()?;

    // Wait for server to start
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // Send SIGTERM
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }

    // Wait up to 5 seconds for clean exit
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if !lifecycle::is_pid_alive(child.id()) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("Server did not exit within 5s after SIGTERM");
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    Ok(())
}
