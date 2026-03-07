//! End-to-end regression tests for agent tool-loop error handling.
//!
//! These tests invoke telegram_reader.py directly to reproduce failure modes that
//! previously caused the agent to hang for 480s (channel budget) instead of
//! stopping early with a clear error.
//!
//! Key regression: `search_global --date-from "2025-01-01"` raised
//! "can't compare offset-naive and offset-aware datetimes" (exit_code=1)
//! causing the agent to retry 3+ times before the LLM gave up thinking → 480s hang.
//!
//! Requirements:
//!   - `TELEGRAM_API_ID`, `TELEGRAM_API_HASH`, `TELEGRAM_PHONE` env vars (or `.env` file)
//!   - Valid Telethon session at `~/.zeroclaw/workspace/skills/telegram-reader/.session/`
//!   - Network access to Telegram API
//!
//! Run:
//!   source .env && cargo test --test agent_tool_error_e2e -- --ignored --test-threads=1

use serde_json::Value;
use std::path::PathBuf;
use std::time::Instant;
use tokio::process::Command;

fn script_path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME env var required");
    PathBuf::from(home)
        .join(".zeroclaw/workspace/skills/telegram-reader/scripts/telegram_reader.py")
}

fn dotenv_path() -> PathBuf {
    if let Ok(explicit) = std::env::var("ZEROCLAW_DOTENV") {
        return PathBuf::from(explicit);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".env")
}

fn load_credentials() -> (String, String, String) {
    if let (Ok(id), Ok(hash), Ok(phone)) = (
        std::env::var("TELEGRAM_API_ID"),
        std::env::var("TELEGRAM_API_HASH"),
        std::env::var("TELEGRAM_PHONE"),
    ) {
        return (id, hash, phone);
    }
    let content = std::fs::read_to_string(dotenv_path()).expect("cannot read .env");
    let mut api_id = String::new();
    let mut api_hash = String::new();
    let mut phone = String::new();
    for line in content.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("TELEGRAM_API_ID=") {
            api_id = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("TELEGRAM_API_HASH=") {
            api_hash = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("TELEGRAM_PHONE=") {
            phone = v.trim().to_string();
        }
    }
    assert!(!api_id.is_empty(), "TELEGRAM_API_ID not found");
    assert!(!api_hash.is_empty(), "TELEGRAM_API_HASH not found");
    assert!(!phone.is_empty(), "TELEGRAM_PHONE not found");
    (api_id, api_hash, phone)
}

async fn run_script(args: &[&str]) -> (Option<i32>, Value, String) {
    let (api_id, api_hash, phone) = load_credentials();
    let output = Command::new("python3")
        .arg(script_path())
        .args(args)
        .env("TELEGRAM_API_ID", &api_id)
        .env("TELEGRAM_API_HASH", &api_hash)
        .env("TELEGRAM_PHONE", &phone)
        .output()
        .await
        .expect("failed to execute telegram_reader.py");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let json = serde_json::from_str(stdout.trim()).unwrap_or(Value::Null);
    (output.status.code(), json, stderr)
}

// ═══════════════════════════════════════════════════════════════════════════
// Regression: datetime offset-naive vs offset-aware crash
// ═══════════════════════════════════════════════════════════════════════════

/// Regression: `search_global --date-from "2025-01-01"` must NOT crash with
/// "can't compare offset-naive and offset-aware datetimes".
///
/// This was the root cause of the 480s agent hang: the script exited with
/// code 1 repeatedly, the LLM retried 3 times, then spent ~6 min thinking
/// before the channel budget (480s) fired.
///
/// Fix: all datetime.fromisoformat() calls must use .replace(tzinfo=timezone.utc)
/// so they are timezone-aware and comparable to message.date (UTC from Telegram).
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn e2e_search_global_date_from_2025_does_not_crash() {
    let started = Instant::now();

    let (exit_code, json, stderr) = run_script(&[
        "search_global",
        "--query",
        "привет",
        "--limit",
        "5",
        "--date-from",
        "2025-01-01",
    ])
    .await;

    let elapsed = started.elapsed();

    assert_eq!(
        exit_code,
        Some(0),
        "script must exit 0 with date-from=2025-01-01\n\
         stderr: {stderr}\n\
         json: {json}"
    );
    assert_eq!(
        json["success"], true,
        "result must be success=true, got: {json}"
    );
    assert!(
        !stderr.contains("offset-naive") && !stderr.contains("offset-aware"),
        "datetime timezone error must not appear in stderr: {stderr}"
    );

    // Script should complete well within 30s — if it hangs it means the bug is back
    assert!(
        elapsed.as_secs() < 30,
        "script took {}s — expected < 30s",
        elapsed.as_secs()
    );
}

/// Regression: same crash via the exact command template used by the daemon skill tool.
///
/// The daemon passes all optional args even when empty: `--date-from {date_from}`.
/// When the LLM sets date_from="2025-01-01" and channel_filter="" and date_to="",
/// this exact invocation must succeed.
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn e2e_search_global_exact_daemon_command_template() {
    let (exit_code, json, stderr) = run_script(&[
        "search_global",
        "--query",
        "мастер",
        "--limit",
        "5",
        "--date-from",
        "2025-01-01",
        "--date-to",
        "",
        "--channel-filter",
        "",
    ])
    .await;

    assert_eq!(
        exit_code,
        Some(0),
        "exact daemon command template must exit 0\n\
         stderr: {stderr}\n\
         json: {json}"
    );
    assert_eq!(
        json["success"], true,
        "result must be success=true, got: {json}"
    );
    assert!(
        !stderr.contains("offset-naive") && !stderr.contains("offset-aware"),
        "datetime timezone error in stderr: {stderr}"
    );
}

/// Regression: date_from filter actually works — results after 2025-01-01 only.
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn e2e_search_global_date_from_filters_old_results() {
    let (exit_code, json, _stderr) = run_script(&[
        "search_global",
        "--query",
        "привет",
        "--limit",
        "20",
        "--date-from",
        "2025-01-01",
    ])
    .await;

    assert_eq!(exit_code, Some(0), "must exit 0: {json}");
    assert_eq!(json["success"], true, "must succeed: {json}");

    let results = json["results"].as_array().expect("results must be array");
    for msg in results {
        let date_str = msg["date"].as_str().expect("date must be string");
        assert!(
            date_str >= "2025-01-01",
            "all results must be >= 2025-01-01 due to date_from filter, got: {date_str}"
        );
    }
}
