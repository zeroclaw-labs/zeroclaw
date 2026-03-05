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

    assert!(
        !api_id.is_empty(),
        "TELEGRAM_API_ID not found in env or .env"
    );
    assert!(
        !api_hash.is_empty(),
        "TELEGRAM_API_HASH not found in env or .env"
    );
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
    cmd.env("PYTHONUSERBASE", format!("{real_home}/.local"));

    let output = cmd
        .output()
        .await
        .expect("Failed to execute telegram_reader.py");

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

    let dialogs = result["dialogs"]
        .as_array()
        .expect("dialogs should be an array");
    assert!(!dialogs.is_empty(), "dialogs array should not be empty");

    // Validate dialog structure
    let first = &dialogs[0];
    assert!(first["id"].is_number(), "dialog.id should be a number");
    assert!(
        first["name"].is_string() || first["name"].is_null(),
        "dialog.name should be string or null"
    );
    assert!(first["type"].is_string(), "dialog.type should be a string");

    let valid_types = ["user", "bot", "group", "channel", "supergroup"];
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
            assert_ne!(
                exit_code,
                Some(0),
                "Should exit non-zero without credentials"
            );
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
    assert!(result["count"].is_number(), "count should be a number");
    assert!(result["messages"].is_array(), "messages should be an array");
}

/// `search_channels` finds channels by name keyword
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn e2e_search_channels_returns_matching_channels() {
    let result = run_telegram_reader(&[
        "search_channels",
        "--channel-query",
        "python",
        "--limit",
        "5",
    ])
    .await;

    assert_eq!(
        result["success"], true,
        "search_channels should succeed, got: {result}"
    );

    assert!(result["count"].is_number(), "count should be a number");
    assert!(result["channels"].is_array(), "channels should be an array");
    assert_eq!(result["query"], "python", "query should be preserved");

    // If we found any channels, validate structure
    if let Some(channels) = result["channels"].as_array() {
        if !channels.is_empty() {
            let first = &channels[0];
            assert!(first["id"].is_number(), "channel.id should be a number");
            assert!(
                first["name"].is_string() || first["name"].is_null(),
                "channel.name should be string or null"
            );
            assert!(first["type"].is_string(), "channel.type should be a string");
        }
    }
}

/// `search_global` with channel_filter performs two-step search
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn e2e_search_global_with_channel_filter() {
    let result = run_telegram_reader(&[
        "search_global",
        "--query",
        "привет",
        "--channel-filter",
        "python",
        "--limit",
        "10",
    ])
    .await;

    assert_eq!(
        result["success"], true,
        "search_global with channel_filter should succeed, got: {result}"
    );

    assert!(result["count"].is_number(), "count should be a number");
    assert!(result["results"].is_array(), "results should be an array");
    assert_eq!(
        result["query"], "привет",
        "message query should be preserved"
    );
    assert_eq!(
        result["channel_filter"], "python",
        "channel_filter should be preserved"
    );
    assert!(
        result["dialogs_scanned"].is_number(),
        "dialogs_scanned should be a number"
    );
}

#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn e2e_search_global_returns_results_from_multiple_chats() {
    let result = run_telegram_reader(&[
        "search_global",
        "--query",
        "привет",
        "--limit",
        "10",
        "--dialogs-limit",
        "10",
    ])
    .await;

    // Validate JSON structure
    assert_eq!(
        result["success"], true,
        "search_global should succeed, got: {result}"
    );
    assert!(result["count"].is_number(), "count should be a number");
    assert!(result["results"].is_array(), "results should be an array");
    assert!(
        result["dialogs_scanned"].is_number(),
        "dialogs_scanned should be a number"
    );
    assert_eq!(
        result["query"], "привет",
        "query should match the search term"
    );

    // Validate result structure if any found
    if result["count"].as_u64().unwrap() > 0 {
        let first_result = &result["results"][0];
        assert!(
            first_result["id"].is_number(),
            "message id should be a number"
        );
        assert!(
            first_result["date"].is_string(),
            "message date should be a string"
        );
        assert!(
            first_result["text"].is_string(),
            "message text should be a string"
        );
        assert!(
            first_result["chat"].is_object(),
            "chat info should be an object"
        );
        assert!(
            first_result["chat"]["name"].is_string(),
            "chat name should be a string"
        );
        assert!(
            first_result["chat"]["type"].is_string(),
            "chat type should be a string"
        );
    }
}

#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn e2e_search_global_with_no_results_returns_empty() {
    let result = run_telegram_reader(&[
        "search_global",
        "--query",
        "xyzqwertynonexistent12345",
        "--limit",
        "5",
        "--dialogs-limit",
        "5",
    ])
    .await;

    assert_eq!(
        result["success"], true,
        "search_global with no results should still succeed, got: {result}"
    );
    assert_eq!(
        result["count"].as_u64().unwrap(),
        0,
        "count should be 0 when no results found"
    );
    assert!(
        result["results"].as_array().unwrap().is_empty(),
        "results array should be empty when no matches"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Bug reproduction tests — real-world agent failures (TDD red phase)
//
// These tests reproduce bugs observed in live agent session 2026-03-04:
// Agent asDrgl failed to search Telegram for "кондиционер Самуи" due to
// multiple cascading failures documented below.
// ═══════════════════════════════════════════════════════════════════════════

/// Bug reproduction: empty string params should be treated as absent by script.
///
/// Real-world failure: tool_handler rendered `--date-from '' --date-to '' --channel-filter ''`
/// because LLM sent `""` for optional params. Even if tool_handler now strips these,
/// the Python script must also be resilient — empty string args from CLI should behave
/// identically to absent args (argparse stores "" not None).
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn e2e_bug_empty_string_params_treated_as_absent() {
    // Simulate pre-fix behavior: explicitly pass empty strings for optional params
    let with_empty = run_telegram_reader(&[
        "search_global",
        "--query",
        "привет",
        "--limit",
        "5",
        "--dialogs-limit",
        "5",
        "--date-from",
        "",
        "--date-to",
        "",
        "--channel-filter",
        "",
    ])
    .await;

    // Baseline: call without optional params at all
    let without = run_telegram_reader(&[
        "search_global",
        "--query",
        "привет",
        "--limit",
        "5",
        "--dialogs-limit",
        "5",
    ])
    .await;

    assert_eq!(
        with_empty["success"], true,
        "should succeed with empty string params, got: {with_empty}"
    );
    assert_eq!(
        without["success"], true,
        "should succeed without optional params, got: {without}"
    );

    // Empty string channel_filter must NOT activate channel filtering
    // (if it does, iter_dialogs scans 200 dialogs matching "" which matches everything)
    assert_eq!(
        with_empty["dialogs_scanned"],
        without["dialogs_scanned"],
        "empty string channel_filter should behave same as absent.\n\
         with empty strings: dialogs_scanned={}, channel_filter={}\n\
         without params:     dialogs_scanned={}, channel_filter={}",
        with_empty["dialogs_scanned"],
        with_empty["channel_filter"],
        without["dialogs_scanned"],
        without["channel_filter"]
    );
}

/// Bug reproduction: search_global stops scanning after first dialog fills limit.
///
/// Real-world failure: `search_global --query "кондиционер" --dialogs-limit 30`
/// returned `dialogs_scanned: 1` — the bot's own chat was first in dialog list,
/// contained 10+ messages mentioning "кондиционер" (the bot's own replies!),
/// and limit=10 was hit immediately. No real group chats were ever searched.
///
/// Expected: search_global should always scan ALL requested dialogs (up to
/// dialogs_limit), collecting per-dialog results, then return top N by date.
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn e2e_bug_search_global_must_scan_all_requested_dialogs() {
    let result = run_telegram_reader(&[
        "search_global",
        "--query",
        "привет",
        "--limit",
        "50",
        "--dialogs-limit",
        "10",
    ])
    .await;

    assert_eq!(
        result["success"], true,
        "search_global should succeed, got: {result}"
    );

    let scanned = result["dialogs_scanned"].as_u64().unwrap();

    // With native global search (SearchGlobalRequest), dialogs_scanned = unique
    // chats that contributed results. For a common word like "привет" across
    // many chats, we expect results from multiple distinct chats.
    assert!(
        scanned >= 2,
        "search_global should return results from multiple chats, \
         but dialogs_scanned={scanned}. This suggests the search is \
         still limited to a single chat.",
    );
}

/// Bug reproduction: search_global results dominated by bot's own messages.
///
/// Real-world failure: all 10 results came from chat `asDrgl` (type=user),
/// which is the bot talking to itself about "кондиционер". No results from
/// actual Telegram groups/channels where real users discuss the topic.
///
/// Expected: results from groups/channels/supergroups should be present
/// when searching a common word across 20 dialogs.
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn e2e_bug_search_global_results_include_group_chats() {
    let result = run_telegram_reader(&[
        "search_global",
        "--query",
        "привет",
        "--limit",
        "30",
        "--dialogs-limit",
        "20",
    ])
    .await;

    assert_eq!(
        result["success"], true,
        "search_global should succeed, got: {result}"
    );

    let results = result["results"]
        .as_array()
        .expect("results should be an array");

    assert!(
        !results.is_empty(),
        "search for common word 'привет' across 20 dialogs should find something"
    );

    // Collect unique chat types from results
    let chat_types: Vec<&str> = results
        .iter()
        .filter_map(|r| r["chat"]["type"].as_str())
        .collect();

    let has_group_results = chat_types
        .iter()
        .any(|t| *t == "group" || *t == "channel" || *t == "supergroup");

    assert!(
        has_group_results,
        "search_global across 20 dialogs for 'привет' should include results from \
         groups/channels/supergroups, but got only these chat types: {chat_types:?}\n\
         First 5 results: {}",
        results
            .iter()
            .take(5)
            .map(|r| format!(
                "  {}({}) from {}",
                r["chat"]["name"], r["chat"]["type"], r["date"]
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Bug reproduction: search_global returns bot-to-user echo messages.
///
/// Real-world failure: the bot searched for "кондиционер" and found its own
/// prior replies to the user: "С текущими инструментами я не могу искать...",
/// "Ок, в Merry Samuistmas! по слову «кондиционер» ничего не нашлось."
/// These are messages FROM the bot TO the user, polluting search results.
///
/// Expected: messages sent by bot accounts (is_bot=true or known bot IDs)
/// should be excluded or deprioritized in search_global results.
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn e2e_bug_search_global_excludes_bot_echo_messages() {
    let result = run_telegram_reader(&[
        "search_global",
        "--query",
        "кондиционер",
        "--limit",
        "20",
        "--dialogs-limit",
        "30",
    ])
    .await;

    assert_eq!(
        result["success"], true,
        "search_global should succeed, got: {result}"
    );

    let results = result["results"]
        .as_array()
        .expect("results should be an array");

    // Count results that are from bot entities
    let bot_results: Vec<_> = results
        .iter()
        .filter(|r| {
            let sender_type = r["sender"]["type"].as_str().unwrap_or("");
            let sender_name = r["sender"]["name"].as_str().unwrap_or("");
            let sender_username = r["sender"]["username"].as_str().unwrap_or("");
            // Bot messages: sender is a bot account, or the chat is a 1-on-1 with a bot
            sender_type == "bot"
                || sender_username.ends_with("_bot")
                || sender_username.ends_with("Bot")
                || sender_name == "asDrgl"
        })
        .collect();

    let total = results.len();
    let bot_count = bot_results.len();

    // Bot messages should not dominate results
    assert!(
        bot_count <= total / 4,
        "Bot echo messages should be <=25% of results, but got {bot_count}/{total}.\n\
         Bot results: {}",
        bot_results
            .iter()
            .take(3)
            .map(|r| format!(
                "  sender={}(@{}) in chat {}",
                r["sender"]["name"], r["sender"]["username"], r["chat"]["name"]
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Bug reproduction: search_channels only searches user's existing dialogs.
///
/// Real-world failure: user asked to search for Samui-related channels globally
/// in Telegram. Agent used search_channels which only calls iter_dialogs —
/// finds only channels the user already joined. No Telegram directory search.
///
/// Expected: search_channels should search Telegram's global directory
/// (via client.search_global or contacts.search) in addition to local dialogs,
/// so it can discover NEW channels the user hasn't joined yet.
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn e2e_bug_search_channels_finds_public_channels() {
    // "python" is common enough to exist in Telegram's global directory
    let result = run_telegram_reader(&[
        "search_channels",
        "--channel-query",
        "python",
        "--limit",
        "200",
    ])
    .await;

    assert_eq!(
        result["success"], true,
        "search_channels should succeed, got: {result}"
    );

    let count = result["count"].as_u64().unwrap();

    // With only iter_dialogs, this returns very few results (only already-joined chats).
    // With global search, "python" should find many public channels/groups.
    assert!(
        count >= 5,
        "search_channels for 'python' should find at least 5 channels \
         (including from Telegram global directory), but found only {count}.\n\
         This suggests search_channels is limited to iter_dialogs (user's existing chats)\n\
         and does not search Telegram's global channel directory.\n\
         Results: {result}"
    );
}

/// Integration scenario: the exact query from the failed agent session.
///
/// Reproduces the full failing scenario: user asks to find AC service on Samui.
/// Agent should search Telegram for relevant channels and messages.
/// This test verifies the search_global pipeline works end-to-end for this query.
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn e2e_scenario_samui_aircon_search_pipeline() {
    // Step 1: Find Samui-related channels
    let channels = run_telegram_reader(&[
        "search_channels",
        "--channel-query",
        "Samui",
        "--limit",
        "200",
    ])
    .await;

    assert_eq!(
        channels["success"], true,
        "channel search should succeed: {channels}"
    );

    let channel_count = channels["count"].as_u64().unwrap();

    // Step 2: Global search for aircon-related messages
    let search =
        run_telegram_reader(&["search_global", "--query", "кондиционер", "--limit", "20"]).await;

    assert_eq!(
        search["success"], true,
        "global search should succeed: {search}"
    );

    let scanned = search["dialogs_scanned"].as_u64().unwrap();
    let result_count = search["count"].as_u64().unwrap();

    // With native global search, dialogs_scanned = unique chats with results
    assert!(
        scanned >= 2,
        "search should return results from multiple chats (scanned {scanned})"
    );

    // Print diagnostic summary for manual review
    eprintln!(
        "\n=== Samui aircon search pipeline results ===\n\
         Channels found for 'Samui': {channel_count}\n\
         Dialogs scanned for 'кондиционер': {scanned}\n\
         Messages found: {result_count}\n\
         Channels: {}\n\
         Top 5 search results:\n{}",
        channels["channels"],
        search["results"]
            .as_array()
            .unwrap()
            .iter()
            .take(5)
            .map(|r| format!(
                "  [{}] {} in {}({}): {}",
                r["date"].as_str().unwrap_or("?"),
                r["sender"]["name"].as_str().unwrap_or("?"),
                r["chat"]["name"].as_str().unwrap_or("?"),
                r["chat"]["type"].as_str().unwrap_or("?"),
                &r["text"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect::<String>()
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
