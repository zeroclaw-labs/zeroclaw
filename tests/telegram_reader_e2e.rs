//! End-to-end integration tests for the telegram-reader skill.
//!
//! These tests invoke the Python `telegram_reader.py` script directly via
//! `tokio::process::Command`, validating JSON output structure and error handling.
//!
//! Requirements:
//!   - `TELEGRAM_API_ID`, `TELEGRAM_API_HASH`, `TELEGRAM_PHONE` env vars (or `.env` file)
//!   - Valid Telethon session at `~/.zeroclaw/workspace/skills/telegram-reader/.session/`
//!   - Network access to Telegram API
//!
//! Run:
//!   source .env && cargo test --test telegram_reader_e2e -- --ignored

use serde_json::Value;
use std::path::PathBuf;
use tokio::process::Command;

/// Resolved path to the telegram_reader.py script.
fn script_path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME env var required");
    PathBuf::from(home)
        .join(".zeroclaw/workspace/skills/telegram-reader/scripts/telegram_reader.py")
}

/// Path to the .env file used as credential fallback.
fn dotenv_path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME env var required");
    PathBuf::from(home).join("work/erp/zeroclaws/.env")
}

/// Load Telegram credentials from environment or .env file.
/// Returns (api_id, api_hash, phone).
fn load_credentials() -> (String, String, String) {
    // Try environment first
    if let (Ok(id), Ok(hash), Ok(phone)) = (
        std::env::var("TELEGRAM_API_ID"),
        std::env::var("TELEGRAM_API_HASH"),
        std::env::var("TELEGRAM_PHONE"),
    ) {
        return (id, hash, phone);
    }

    // Fallback: parse .env file
    let env_path = dotenv_path();
    let content = std::fs::read_to_string(&env_path)
        .unwrap_or_else(|e| panic!("Cannot read .env at {}: {e}", env_path.display()));

    let mut api_id = String::new();
    let mut api_hash = String::new();
    let mut phone = String::new();

    for line in content.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("TELEGRAM_API_ID=") {
            api_id = val.trim().to_string();
        } else if let Some(val) = line.strip_prefix("TELEGRAM_API_HASH=") {
            api_hash = val.trim().to_string();
        } else if let Some(val) = line.strip_prefix("TELEGRAM_PHONE=") {
            phone = val.trim().to_string();
        }
    }

    assert!(!api_id.is_empty(), "TELEGRAM_API_ID not found in env or .env");
    assert!(!api_hash.is_empty(), "TELEGRAM_API_HASH not found in env or .env");
    assert!(!phone.is_empty(), "TELEGRAM_PHONE not found in env or .env");

    (api_id, api_hash, phone)
}

/// Run the telegram_reader.py script with given arguments and return parsed JSON.
async fn run_telegram_reader(args: &[&str]) -> Value {
    let (api_id, api_hash, phone) = load_credentials();

    let output = Command::new("python3")
        .arg(script_path())
        .args(args)
        .env("TELEGRAM_API_ID", &api_id)
        .env("TELEGRAM_API_HASH", &api_hash)
        .env("TELEGRAM_PHONE", &phone)
        .output()
        .await
        .expect("Failed to execute telegram_reader.py");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "Failed to parse JSON from telegram_reader.py\n\
             exit code: {:?}\n\
             stdout: {stdout}\n\
             stderr: {stderr}\n\
             parse error: {e}",
            output.status.code()
        )
    })
}

/// Run the telegram_reader.py script without valid Telegram credentials.
/// Uses a fake HOME to prevent .env fallback, but preserves PATH and
/// Python paths so the interpreter and packages remain accessible.
/// Returns (exit_code, stdout, stderr).
async fn run_telegram_reader_no_creds(args: &[&str]) -> (Option<i32>, String, String) {
    let fake_home = "/tmp/telegram_reader_e2e_nocreds";

    let mut cmd = Command::new("python3");
    cmd.arg(script_path())
        .args(args)
        // Clear Telegram vars
        .env_remove("TELEGRAM_API_ID")
        .env_remove("TELEGRAM_API_HASH")
        .env_remove("TELEGRAM_PHONE")
        // Fake HOME so .env fallback path doesn't resolve to real credentials
        .env("HOME", fake_home);

    // Preserve PATH so python3 and system libs are found
    if let Ok(path) = std::env::var("PATH") {
        cmd.env("PATH", path);
    }
    // Preserve Python package paths (user-installed telethon etc.)
    if let Ok(pp) = std::env::var("PYTHONPATH") {
        cmd.env("PYTHONPATH", pp);
    }
    // Preserve the real user site-packages by pointing Python to actual home
    let real_home = std::env::var("HOME").unwrap_or_default();
    cmd.env(
        "PYTHONUSERBASE",
        format!("{real_home}/.local"),
    );

    let output = cmd.output().await.expect("Failed to execute telegram_reader.py");

    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

// ═══════════════════════════════════════════════════════════════════════════
// Test cases
// ═══════════════════════════════════════════════════════════════════════════

/// Smoke test: `list_dialogs` returns valid JSON with expected structure.
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn e2e_list_dialogs_returns_valid_json() {
    let result = run_telegram_reader(&["list_dialogs", "--limit", "5"]).await;

    assert_eq!(
        result["success"], true,
        "list_dialogs should succeed, got: {result}"
    );
    assert!(
        result["count"].as_u64().unwrap() > 0,
        "Expected at least one dialog, got count={}",
        result["count"]
    );

    let dialogs = result["dialogs"].as_array().expect("dialogs should be an array");
    assert!(!dialogs.is_empty(), "dialogs array should not be empty");

    // Validate dialog structure
    let first = &dialogs[0];
    assert!(first["id"].is_number(), "dialog.id should be a number");
    assert!(
        first["name"].is_string() || first["name"].is_null(),
        "dialog.name should be string or null"
    );
    assert!(first["type"].is_string(), "dialog.type should be a string");

    let valid_types = ["user", "group", "channel", "supergroup"];
    let dtype = first["type"].as_str().unwrap();
    assert!(
        valid_types.contains(&dtype),
        "dialog.type should be one of {valid_types:?}, got: {dtype}"
    );
}

/// `search_messages` with a known contact returns valid message structure.
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn e2e_search_messages_with_contact() {
    let result = run_telegram_reader(&[
        "search_messages",
        "--contact-name",
        "zverozabr",
        "--limit",
        "3",
    ])
    .await;

    assert_eq!(
        result["success"], true,
        "search_messages should succeed, got: {result}"
    );
    assert!(
        result["count"].as_u64().unwrap() > 0,
        "Expected at least one message"
    );

    // Validate chat metadata
    let chat = &result["chat"];
    assert!(chat["id"].is_number(), "chat.id should be a number");
    assert!(chat["type"].is_string(), "chat.type should be a string");

    // Validate message structure
    let messages = result["messages"]
        .as_array()
        .expect("messages should be an array");
    assert!(!messages.is_empty(), "messages array should not be empty");

    let msg = &messages[0];
    assert!(msg["id"].is_number(), "message.id should be a number");
    assert!(msg["date"].is_string(), "message.date should be a string");
    assert!(
        msg["text"].is_string(),
        "message.text should be a string (possibly empty)"
    );
    assert!(
        msg["sender_id"].is_number(),
        "message.sender_id should be a number"
    );
}

/// `search_messages` with a nonexistent contact returns error.
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn e2e_search_invalid_contact_returns_error() {
    let result = run_telegram_reader(&[
        "search_messages",
        "--contact-name",
        "totally_nonexistent_user_xyz_99999",
        "--limit",
        "1",
    ])
    .await;

    assert_eq!(
        result["success"], false,
        "search with invalid contact should fail, got: {result}"
    );
    assert!(
        result["error"].is_string(),
        "error field should be a string"
    );
    assert!(
        !result["error"].as_str().unwrap().is_empty(),
        "error message should not be empty"
    );
}

/// Running without credentials (no env vars, no .env fallback) should fail gracefully.
#[tokio::test]
#[ignore = "requires network"]
async fn e2e_missing_credentials_returns_error() {
    let (exit_code, stdout, stderr) =
        run_telegram_reader_no_creds(&["list_dialogs", "--limit", "1"]).await;

    assert_ne!(
        exit_code,
        Some(0),
        "Should exit with non-zero when credentials are missing"
    );

    // The script should output JSON error to stdout or stderr
    let combined = format!("{stdout}{stderr}");
    let parsed: Result<Value, _> = serde_json::from_str(combined.trim());

    match parsed {
        Ok(json) => {
            assert_eq!(
                json["success"], false,
                "Should report success=false without credentials"
            );
            let error_msg = json["error"].as_str().unwrap_or("");
            assert!(
                error_msg.contains("TELEGRAM_API_ID")
                    || error_msg.contains("API")
                    || error_msg.contains("environment"),
                "Error should mention missing credentials, got: {error_msg}"
            );
        }
        Err(_) => {
            // If not valid JSON, at least verify non-zero exit
            assert_ne!(exit_code, Some(0), "Should exit non-zero without credentials");
        }
    }
}

/// `search_messages` with a keyword query returns filtered results.
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn e2e_search_messages_with_query() {
    // Search in Saved Messages (own account) for a common word
    let result = run_telegram_reader(&[
        "search_messages",
        "--contact-name",
        "zverozabr",
        "--query",
        "юрист",
        "--limit",
        "3",
    ])
    .await;

    assert_eq!(
        result["success"], true,
        "search_messages with query should succeed, got: {result}"
    );

    // count may be 0 if no messages match, but structure should be valid
    assert!(
        result["count"].is_number(),
        "count should be a number"
    );
    assert!(
        result["messages"].is_array(),
        "messages should be an array"
    );
}
