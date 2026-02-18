use anyhow::Result;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use zeroclaw::cp;
use zeroclaw::db::{AgentEvent, AgentUsageRecord, Registry};

/// Helper: create a registry with a registered instance in a temp dir.
/// Returns (TempDir, db_path, instance_id, instance_dir).
fn setup_instance(name: &str, port: u16, config_toml: &str) -> (TempDir, PathBuf, String, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let cp_dir = tmp.path().join("cp");
    let instances_dir = cp_dir.join("instances");
    fs::create_dir_all(&instances_dir).unwrap();

    let db_path = cp_dir.join("registry.db");
    let registry = Registry::open(&db_path).unwrap();

    let id = uuid::Uuid::new_v4().to_string();
    let inst_dir = instances_dir.join(&id);
    fs::create_dir_all(&inst_dir).unwrap();

    let config_path = inst_dir.join("config.toml");
    fs::write(&config_path, config_toml).unwrap();

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
async fn start_test_server(db_path: PathBuf) -> (String, tokio::sync::watch::Sender<bool>) {
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

/// Config TOML with ALL secret fields populated with unique SECRET_* values.
fn full_secrets_config() -> String {
    r#"
api_key = "SECRET_TOP_LEVEL_API_KEY"
default_temperature = 0.7

[gateway]
port = 18900
host = "127.0.0.1"
require_pairing = true
paired_tokens = ["SECRET_PAIRED_TOKEN_1", "SECRET_PAIRED_TOKEN_2"]

[channels_config]
cli = true

[channels_config.telegram]
bot_token = "SECRET_TELEGRAM_BOT_TOKEN"
allowed_users = ["alice"]

[channels_config.discord]
bot_token = "SECRET_DISCORD_BOT_TOKEN"
guild_id = "123456"

[channels_config.slack]
bot_token = "SECRET_SLACK_BOT_TOKEN"
app_token = "SECRET_SLACK_APP_TOKEN"

[channels_config.webhook]
port = 9090
secret = "SECRET_WEBHOOK_SECRET"

[channels_config.matrix]
homeserver = "https://matrix.org"
access_token = "SECRET_MATRIX_ACCESS_TOKEN"
room_id = "!room:matrix.org"
allowed_users = ["@user:matrix.org"]

[channels_config.whatsapp]
access_token = "SECRET_WHATSAPP_ACCESS_TOKEN"
phone_number_id = "12345"
verify_token = "SECRET_WHATSAPP_VERIFY_TOKEN"
app_secret = "SECRET_WHATSAPP_APP_SECRET"

[channels_config.irc]
server = "irc.example.com"
port = 6697
nickname = "testbot"
server_password = "SECRET_IRC_SERVER_PASSWORD"
nickserv_password = "SECRET_IRC_NICKSERV_PASSWORD"
sasl_password = "SECRET_IRC_SASL_PASSWORD"

[channels_config.email]
imap_host = "imap.example.com"
smtp_host = "smtp.example.com"
username = "user@example.com"
password = "SECRET_EMAIL_PASSWORD"
from_address = "bot@example.com"

[composio]
enabled = true
api_key = "SECRET_COMPOSIO_API_KEY"

[tunnel]
provider = "ngrok"

[tunnel.ngrok]
auth_token = "SECRET_NGROK_AUTH_TOKEN"

[tunnel.cloudflare]
token = "SECRET_CLOUDFLARE_TOKEN"

[[model_routes]]
hint = "fast"
provider = "groq"
model = "llama-3"
api_key = "SECRET_MODEL_ROUTE_API_KEY"

[[model_routes]]
hint = "reason"
provider = "openai"
model = "gpt-4o"
"#
    .to_string()
}

/// All known SECRET_* values that must not appear in the /details response.
fn all_secret_values() -> Vec<&'static str> {
    vec![
        "SECRET_TOP_LEVEL_API_KEY",
        "SECRET_PAIRED_TOKEN_1",
        "SECRET_PAIRED_TOKEN_2",
        "SECRET_TELEGRAM_BOT_TOKEN",
        "SECRET_DISCORD_BOT_TOKEN",
        "SECRET_SLACK_BOT_TOKEN",
        "SECRET_SLACK_APP_TOKEN",
        "SECRET_WEBHOOK_SECRET",
        "SECRET_MATRIX_ACCESS_TOKEN",
        "SECRET_WHATSAPP_ACCESS_TOKEN",
        "SECRET_WHATSAPP_VERIFY_TOKEN",
        "SECRET_WHATSAPP_APP_SECRET",
        "SECRET_IRC_SERVER_PASSWORD",
        "SECRET_IRC_NICKSERV_PASSWORD",
        "SECRET_IRC_SASL_PASSWORD",
        "SECRET_EMAIL_PASSWORD",
        "SECRET_COMPOSIO_API_KEY",
        "SECRET_NGROK_AUTH_TOKEN",
        "SECRET_CLOUDFLARE_TOKEN",
        "SECRET_MODEL_ROUTE_API_KEY",
    ]
}

// ══════════════════════════════════════════════════════════════════
// Gate 1: Details with masked secrets
// ══════════════════════════════════════════════════════════════════

#[tokio::test]
async fn gate1_details_returns_masked_config() -> Result<()> {
    let (_tmp, db_path, _id, _inst_dir) =
        setup_instance("secret-agent", 18950, &full_secrets_config());

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base_url}/api/instances/secret-agent/details"))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await?;

    // Verify structure
    assert!(body.get("instance").is_some(), "Missing 'instance' section");
    assert!(body.get("config").is_some(), "Missing 'config' section");
    assert!(body.get("identity").is_some(), "Missing 'identity' section");
    assert!(body.get("channels").is_some(), "Missing 'channels' section");
    assert!(body.get("model").is_some(), "Missing 'model' section");
    assert!(body.get("runtime").is_some(), "Missing 'runtime' section");

    // Instance metadata
    assert_eq!(body["instance"]["name"], "secret-agent");
    assert_eq!(body["instance"]["port"], 18950);

    // Config should be present (no parse error)
    assert!(body["config"].is_object(), "config should be a JSON object");

    // Non-secret fields preserved
    assert_eq!(body["config"]["default_temperature"], 0.7);

    // Secret fields masked
    assert_eq!(body["config"]["api_key"], "***MASKED***");
    assert_eq!(
        body["config"]["channels_config"]["telegram"]["bot_token"],
        "***MASKED***"
    );

    let _ = shutdown.send(true);
    Ok(())
}

#[tokio::test]
async fn gate1_details_unknown_instance_404() -> Result<()> {
    let tmp = TempDir::new()?;
    let cp_dir = tmp.path().join("cp");
    fs::create_dir_all(cp_dir.join("instances"))?;
    let db_path = cp_dir.join("registry.db");
    let _registry = Registry::open(&db_path)?;

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base_url}/api/instances/nonexistent/details"))
        .send()
        .await?;
    assert_eq!(resp.status(), 404);

    let _ = shutdown.send(true);
    Ok(())
}

#[tokio::test]
async fn gate1_details_missing_config_reports_error() -> Result<()> {
    let tmp = TempDir::new()?;
    let cp_dir = tmp.path().join("cp");
    let instances_dir = cp_dir.join("instances");
    fs::create_dir_all(&instances_dir)?;

    let db_path = cp_dir.join("registry.db");
    let registry = Registry::open(&db_path)?;

    let id = uuid::Uuid::new_v4().to_string();
    let inst_dir = instances_dir.join(&id);
    fs::create_dir_all(&inst_dir)?;

    // Register with a config path that doesn't exist
    let fake_path = inst_dir.join("config.toml");
    registry.create_instance(
        &id,
        "no-config",
        18951,
        fake_path.to_str().unwrap(),
        None,
        None,
    )?;
    // Don't create the file

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base_url}/api/instances/no-config/details"))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await?;
    assert!(body["config"].is_null(), "config should be null when file missing");
    assert!(
        body["config_error"].as_str().is_some(),
        "Should have config_error"
    );

    let _ = shutdown.send(true);
    Ok(())
}

// ══════════════════════════════════════════════════════════════════
// Gate 2: Tasks with stable ordering
// ══════════════════════════════════════════════════════════════════

#[tokio::test]
async fn gate2_tasks_empty_returns_data_available_false() -> Result<()> {
    let (_tmp, db_path, _id, _inst_dir) =
        setup_instance("task-agent", 18952, "default_temperature = 0.7\n");

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base_url}/api/instances/task-agent/tasks"))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await?;
    assert_eq!(body["data_available"], false);
    assert_eq!(body["total"], 0);
    assert!(body["tasks"].as_array().unwrap().is_empty());
    assert!(body["message"].as_str().is_some());

    let _ = shutdown.send(true);
    Ok(())
}

#[tokio::test]
async fn gate2_tasks_ordering_and_pagination() -> Result<()> {
    let (_tmp, db_path, id, _inst_dir) =
        setup_instance("task-order", 18953, "default_temperature = 0.7\n");

    // Insert 5 events
    let registry = Registry::open(&db_path)?;
    for i in 1..=5 {
        registry.insert_agent_event(&AgentEvent {
            id: format!("evt-{i}"),
            instance_id: id.clone(),
            event_type: "tool_call".to_string(),
            channel: Some("cli".to_string()),
            summary: Some(format!("Event {i}")),
            status: "completed".to_string(),
            duration_ms: Some(100 * i as i64),
            correlation_id: None,
            metadata: None,
            created_at: format!("2026-01-01 00:00:0{i}"),
        })?;
    }
    drop(registry);

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    // Request limit=3, should get events 5,4,3 (descending)
    let resp = client
        .get(format!(
            "{base_url}/api/instances/task-order/tasks?limit=3"
        ))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await?;
    assert_eq!(body["data_available"], true);
    assert_eq!(body["total"], 5);
    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 3);
    assert_eq!(tasks[0]["summary"], "Event 5");
    assert_eq!(tasks[1]["summary"], "Event 4");
    assert_eq!(tasks[2]["summary"], "Event 3");

    // Stability: same query returns same results
    let resp2 = client
        .get(format!(
            "{base_url}/api/instances/task-order/tasks?limit=3"
        ))
        .send()
        .await?;
    let body2: serde_json::Value = resp2.json().await?;
    assert_eq!(body["tasks"], body2["tasks"], "Ordering should be stable");

    let _ = shutdown.send(true);
    Ok(())
}

#[tokio::test]
async fn gate2_tasks_invalid_params() -> Result<()> {
    let (_tmp, db_path, _id, _inst_dir) =
        setup_instance("task-bad", 18954, "default_temperature = 0.7\n");

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    // limit too high
    let resp = client
        .get(format!(
            "{base_url}/api/instances/task-bad/tasks?limit=9999"
        ))
        .send()
        .await?;
    assert_eq!(resp.status(), 400);

    // bad status
    let resp = client
        .get(format!(
            "{base_url}/api/instances/task-bad/tasks?status=bogus"
        ))
        .send()
        .await?;
    assert_eq!(resp.status(), 400);

    let _ = shutdown.send(true);
    Ok(())
}

// ══════════════════════════════════════════════════════════════════
// Gate 3: Usage with unknown-data markers
// ══════════════════════════════════════════════════════════════════

#[tokio::test]
async fn gate3_usage_empty_returns_data_available_false() -> Result<()> {
    let (_tmp, db_path, _id, _inst_dir) =
        setup_instance("usage-empty", 18955, "default_temperature = 0.7\n");

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!(
            "{base_url}/api/instances/usage-empty/usage?window=24h"
        ))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await?;
    assert_eq!(body["data_available"], false);
    assert_eq!(body["usage"]["request_count"], 0);
    assert!(body["usage"]["total_tokens"].is_null());

    let _ = shutdown.send(true);
    Ok(())
}

#[tokio::test]
async fn gate3_usage_with_data_and_unknown_count() -> Result<()> {
    let (_tmp, db_path, id, _inst_dir) =
        setup_instance("usage-data", 18956, "default_temperature = 0.7\n");

    let registry = Registry::open(&db_path)?;
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // Record with known tokens
    registry.insert_agent_usage(&AgentUsageRecord {
        id: "u1".to_string(),
        instance_id: id.clone(),
        input_tokens: Some(100),
        output_tokens: Some(200),
        total_tokens: Some(300),
        provider: Some("openai".to_string()),
        model: Some("gpt-4o".to_string()),
        request_id: None,
        created_at: now.clone(),
    })?;

    // Record with known tokens
    registry.insert_agent_usage(&AgentUsageRecord {
        id: "u2".to_string(),
        instance_id: id.clone(),
        input_tokens: Some(50),
        output_tokens: Some(75),
        total_tokens: Some(125),
        provider: Some("anthropic".to_string()),
        model: Some("claude".to_string()),
        request_id: None,
        created_at: now.clone(),
    })?;

    // Record with NULL tokens (unknown)
    registry.insert_agent_usage(&AgentUsageRecord {
        id: "u3".to_string(),
        instance_id: id.clone(),
        input_tokens: None,
        output_tokens: None,
        total_tokens: None,
        provider: Some("local".to_string()),
        model: Some("llama".to_string()),
        request_id: None,
        created_at: now,
    })?;
    drop(registry);

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!(
            "{base_url}/api/instances/usage-data/usage?window=24h"
        ))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await?;
    assert_eq!(body["data_available"], true);
    assert_eq!(body["usage"]["request_count"], 3);
    assert_eq!(body["usage"]["unknown_count"], 1);
    assert_eq!(body["usage"]["input_tokens"], 150);
    assert_eq!(body["usage"]["output_tokens"], 275);
    assert_eq!(body["usage"]["total_tokens"], 425);

    let _ = shutdown.send(true);
    Ok(())
}

#[tokio::test]
async fn gate3_usage_invalid_window() -> Result<()> {
    let (_tmp, db_path, _id, _inst_dir) =
        setup_instance("usage-bad", 18957, "default_temperature = 0.7\n");

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!(
            "{base_url}/api/instances/usage-bad/usage?window=99d"
        ))
        .send()
        .await?;
    assert_eq!(resp.status(), 400);

    let _ = shutdown.send(true);
    Ok(())
}

// ══════════════════════════════════════════════════════════════════
// Gate 4: Enhanced logs (pagination + download)
// ══════════════════════════════════════════════════════════════════

#[tokio::test]
async fn gate4_logs_tail_mode() -> Result<()> {
    let (_tmp, db_path, _id, inst_dir) =
        setup_instance("log-tail", 18958, "default_temperature = 0.7\n");

    let log_dir = inst_dir.join("logs");
    fs::create_dir_all(&log_dir)?;
    let content: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    fs::write(log_dir.join("daemon.log"), content)?;

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!(
            "{base_url}/api/instances/log-tail/logs?lines=5&mode=tail"
        ))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await?;
    assert_eq!(body["mode"], "tail");
    let lines = body["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 5);
    assert_eq!(lines[0], "line 16");
    assert_eq!(lines[4], "line 20");

    let _ = shutdown.send(true);
    Ok(())
}

#[tokio::test]
async fn gate4_logs_page_mode() -> Result<()> {
    let (_tmp, db_path, _id, inst_dir) =
        setup_instance("log-page", 18959, "default_temperature = 0.7\n");

    let log_dir = inst_dir.join("logs");
    fs::create_dir_all(&log_dir)?;
    let content: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    fs::write(log_dir.join("daemon.log"), content)?;

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!(
            "{base_url}/api/instances/log-page/logs?mode=page&offset=5&lines=3"
        ))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await?;
    assert_eq!(body["mode"], "page");
    assert_eq!(body["offset"], 5);
    assert_eq!(body["window_lines"], 20);
    assert_eq!(body["has_more"], true);
    assert_eq!(body["truncated"], false);

    let lines = body["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 3);
    // Lines 6, 7, 8 (0-indexed offset 5 = line 6)
    assert_eq!(lines[0], "line 6");
    assert_eq!(lines[1], "line 7");
    assert_eq!(lines[2], "line 8");

    let _ = shutdown.send(true);
    Ok(())
}

#[tokio::test]
async fn gate4_logs_invalid_mode() -> Result<()> {
    let (_tmp, db_path, _id, _inst_dir) =
        setup_instance("log-bad", 18960, "default_temperature = 0.7\n");

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!(
            "{base_url}/api/instances/log-bad/logs?mode=bogus"
        ))
        .send()
        .await?;
    assert_eq!(resp.status(), 400);

    let _ = shutdown.send(true);
    Ok(())
}

#[tokio::test]
async fn gate4_logs_download() -> Result<()> {
    let (_tmp, db_path, _id, inst_dir) =
        setup_instance("log-dl", 18961, "default_temperature = 0.7\n");

    let log_dir = inst_dir.join("logs");
    fs::create_dir_all(&log_dir)?;
    let content = "hello from log\nsecond line\n";
    fs::write(log_dir.join("daemon.log"), content)?;

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!(
            "{base_url}/api/instances/log-dl/logs/download"
        ))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);

    // Content-Type should be text/plain
    let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(ct.contains("text/plain"));

    // Content-Disposition should be attachment
    let cd = resp
        .headers()
        .get("content-disposition")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cd.contains("attachment"));

    // Body matches file content
    let body = resp.text().await?;
    assert_eq!(body, content);

    let _ = shutdown.send(true);
    Ok(())
}

#[tokio::test]
async fn gate4_logs_download_empty_file() -> Result<()> {
    let (_tmp, db_path, _id, _inst_dir) =
        setup_instance("log-dl-empty", 18962, "default_temperature = 0.7\n");

    // No log file created

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!(
            "{base_url}/api/instances/log-dl-empty/logs/download"
        ))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);
    let body = resp.text().await?;
    assert!(body.is_empty(), "Download of missing log should return empty body");

    let _ = shutdown.send(true);
    Ok(())
}

#[tokio::test]
async fn gate4_logs_download_concatenates_rotated_and_current() -> Result<()> {
    let (_tmp, db_path, _id, inst_dir) =
        setup_instance("log-dl-concat", 18965, "default_temperature = 0.7\n");

    let log_dir = inst_dir.join("logs");
    fs::create_dir_all(&log_dir)?;

    // Rotated log (older content)
    let rotated_content = "old line 1\nold line 2\n";
    fs::write(log_dir.join("daemon.log.1"), rotated_content)?;

    // Current log (newer content)
    let current_content = "new line 1\nnew line 2\n";
    fs::write(log_dir.join("daemon.log"), current_content)?;

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!(
            "{base_url}/api/instances/log-dl-concat/logs/download"
        ))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);

    let body = resp.text().await?;
    // Should be rotated first (chronological), then current
    let expected = format!("{rotated_content}{current_content}");
    assert_eq!(body, expected, "Download should concatenate rotated + current in order");

    let _ = shutdown.send(true);
    Ok(())
}

#[tokio::test]
async fn gate4_logs_download_rotated_only() -> Result<()> {
    let (_tmp, db_path, _id, inst_dir) =
        setup_instance("log-dl-rot", 18966, "default_temperature = 0.7\n");

    let log_dir = inst_dir.join("logs");
    fs::create_dir_all(&log_dir)?;

    // Only rotated log, no current
    let rotated_content = "rotated only\n";
    fs::write(log_dir.join("daemon.log.1"), rotated_content)?;

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!(
            "{base_url}/api/instances/log-dl-rot/logs/download"
        ))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);

    let body = resp.text().await?;
    assert_eq!(body, rotated_content);

    let _ = shutdown.send(true);
    Ok(())
}

// ══════════════════════════════════════════════════════════════════
// Gate 5: Config secrets never appear in details response
// ══════════════════════════════════════════════════════════════════

#[tokio::test]
async fn gate5_no_raw_secrets_in_details_response() -> Result<()> {
    let (_tmp, db_path, _id, _inst_dir) =
        setup_instance("leak-test", 18963, &full_secrets_config());

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base_url}/api/instances/leak-test/details"))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await?;

    // Serialize entire response to string and check for secret values
    let body_str = serde_json::to_string(&body).unwrap();

    for secret in all_secret_values() {
        assert!(
            !body_str.contains(secret),
            "LEAK DETECTED: secret value '{secret}' found in /details response"
        );
    }

    // At least one ***MASKED*** should be present
    assert!(
        body_str.contains("***MASKED***"),
        "Expected at least one ***MASKED*** in response"
    );

    let _ = shutdown.send(true);
    Ok(())
}

#[tokio::test]
async fn gate5_null_secrets_stay_null() -> Result<()> {
    // Config with only some fields, others will be null/missing
    let minimal_config = r#"
default_temperature = 0.7
"#;
    let (_tmp, db_path, _id, _inst_dir) =
        setup_instance("null-secrets", 18964, minimal_config);

    let (base_url, shutdown) = start_test_server(db_path).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base_url}/api/instances/null-secrets/details"))
        .send()
        .await?;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await?;

    // api_key should be null (not "***MASKED***")
    assert!(
        body["config"]["api_key"].is_null(),
        "Null api_key should stay null, not be masked"
    );

    let _ = shutdown.send(true);
    Ok(())
}

// ══════════════════════════════════════════════════════════════════
// Schema migration: unique active-name index
// ══════════════════════════════════════════════════════════════════

#[test]
fn unique_active_name_index_prevents_duplicates() -> Result<()> {
    let tmp = TempDir::new()?;
    let db_path = tmp.path().join("test.db");
    let registry = Registry::open(&db_path)?;

    registry.create_instance("id-1", "agent", 18801, "/c.toml", None, None)?;

    // Second active instance with same name should fail (unique index)
    let result = registry.create_instance("id-2", "agent", 18802, "/c2.toml", None, None);
    assert!(result.is_err(), "Duplicate active name should be rejected");

    // But archived + new active should be fine
    registry
        .conn()
        .execute(
            "UPDATE instances SET archived_at = datetime('now') WHERE id = 'id-1'",
            [],
        )?;
    let result = registry.create_instance("id-3", "agent", 18803, "/c3.toml", None, None);
    assert!(result.is_ok(), "New active instance with archived duplicate name should succeed");

    Ok(())
}

// ══════════════════════════════════════════════════════════════════
// DB methods: agent events + usage
// ══════════════════════════════════════════════════════════════════

#[test]
fn agent_events_crud() -> Result<()> {
    let tmp = TempDir::new()?;
    let db_path = tmp.path().join("test.db");
    let registry = Registry::open(&db_path)?;
    registry.create_instance("inst-1", "agent", 18801, "/c.toml", None, None)?;

    // Insert events
    for i in 1..=3 {
        registry.insert_agent_event(&AgentEvent {
            id: format!("e{i}"),
            instance_id: "inst-1".to_string(),
            event_type: "tool_call".to_string(),
            channel: Some("cli".to_string()),
            summary: Some(format!("Event {i}")),
            status: "completed".to_string(),
            duration_ms: Some(i * 100),
            correlation_id: None,
            metadata: None,
            created_at: format!("2026-01-01 00:00:0{i}"),
        })?;
    }

    let (events, total) = registry.list_agent_events("inst-1", 10, 0, None, None, None)?;
    assert_eq!(total, 3);
    assert_eq!(events.len(), 3);
    // Descending order
    assert_eq!(events[0].id, "e3");
    assert_eq!(events[2].id, "e1");

    Ok(())
}

#[test]
fn agent_usage_aggregation() -> Result<()> {
    let tmp = TempDir::new()?;
    let db_path = tmp.path().join("test.db");
    let registry = Registry::open(&db_path)?;
    registry.create_instance("inst-1", "agent", 18801, "/c.toml", None, None)?;

    registry.insert_agent_usage(&AgentUsageRecord {
        id: "u1".to_string(),
        instance_id: "inst-1".to_string(),
        input_tokens: Some(100),
        output_tokens: Some(200),
        total_tokens: Some(300),
        provider: Some("openai".to_string()),
        model: Some("gpt-4".to_string()),
        request_id: None,
        created_at: "2026-01-01 12:00:00".to_string(),
    })?;

    registry.insert_agent_usage(&AgentUsageRecord {
        id: "u2".to_string(),
        instance_id: "inst-1".to_string(),
        input_tokens: None,
        output_tokens: None,
        total_tokens: None,
        provider: None,
        model: None,
        request_id: None,
        created_at: "2026-01-01 12:01:00".to_string(),
    })?;

    let summary = registry.get_agent_usage("inst-1", None, None)?;
    assert_eq!(summary.request_count, 2);
    assert_eq!(summary.unknown_count, 1);
    assert_eq!(summary.input_tokens, Some(100));
    assert_eq!(summary.output_tokens, Some(200));
    assert_eq!(summary.total_tokens, Some(300));

    Ok(())
}
