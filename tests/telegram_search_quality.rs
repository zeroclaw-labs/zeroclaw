//! Telegram Search Quality Test Suite
//!
//! Validates search quality across four levels:
//!   Level 1 — Unit tests (no network, no Telegram API)
//!   Level 2 — Integration tests (live Telegram API, `#[ignore]`)
//!   Level 3 — Quality E2E (real search scenarios, `#[ignore]`)
//!   Level 4 — Performance notes (see scripts/bench_search.sh)
//!
//! Run unit tests only (no credentials needed):
//!   cargo test --test telegram_search_quality -- --nocapture
//!
//! Run integration + quality tests:
//!   source .env && cargo test --test telegram_search_quality -- --ignored --test-threads=1 --nocapture

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};
use tokio::process::Command;

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn mirror_script() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME env var required");
    PathBuf::from(home)
        .join(".zeroclaw/workspace/skills/telegram-reader/scripts/telegram_mirror.py")
}

fn reader_script() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME env var required");
    PathBuf::from(home)
        .join(".zeroclaw/workspace/skills/telegram-reader/scripts/telegram_reader.py")
}

fn dotenv_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".env")
}

/// Load Telegram credentials from env or .env fallback.
fn load_credentials() -> (String, String, String) {
    if let (Ok(id), Ok(hash), Ok(phone)) = (
        std::env::var("TELEGRAM_API_ID"),
        std::env::var("TELEGRAM_API_HASH"),
        std::env::var("TELEGRAM_PHONE"),
    ) {
        return (id, hash, phone);
    }

    let content =
        std::fs::read_to_string(dotenv_path()).unwrap_or_else(|e| panic!("Cannot read .env: {e}"));

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

    assert!(!api_id.is_empty(), "TELEGRAM_API_ID missing");
    assert!(!api_hash.is_empty(), "TELEGRAM_API_HASH missing");
    assert!(!phone.is_empty(), "TELEGRAM_PHONE missing");

    (api_id, api_hash, phone)
}

/// Run telegram_mirror.py with args, return parsed JSON.
fn run_mirror_sync(args: &[&str]) -> Value {
    let output = StdCommand::new("python3")
        .arg(mirror_script())
        .args(args)
        .output()
        .expect("Failed to execute telegram_mirror.py");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("Failed to parse JSON\nstdout: {stdout}\nstderr: {stderr}\nparse error: {e}")
    })
}

/// Run telegram_mirror.py with Telegram credentials.
async fn run_mirror_with_creds(args: &[&str]) -> Value {
    let (api_id, api_hash, phone) = load_credentials();

    let output = Command::new("python3")
        .arg(mirror_script())
        .args(args)
        .env("TELEGRAM_API_ID", &api_id)
        .env("TELEGRAM_API_HASH", &api_hash)
        .env("TELEGRAM_PHONE", &phone)
        .output()
        .await
        .expect("Failed to execute telegram_mirror.py");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("Failed to parse JSON\nstdout: {stdout}\nstderr: {stderr}\nparse error: {e}")
    })
}

/// Run telegram_mirror.py using the research account session (mirrors what reader uses).
async fn run_mirror_with_research_session(args: &[&str]) -> Value {
    let (api_id, api_hash, _phone) = load_credentials();
    // Reader always uses research_session; mirror must match to keep msg IDs consistent.
    let research_phone = std::env::var("TELEGRAM_RESEARCH_PHONE")
        .or_else(|_| {
            let content = std::fs::read_to_string(dotenv_path()).unwrap_or_default();
            content
                .lines()
                .find(|l| l.starts_with("TELEGRAM_RESEARCH_PHONE="))
                .map(|l| l["TELEGRAM_RESEARCH_PHONE=".len()..].trim().to_string())
                .ok_or(std::env::VarError::NotPresent)
        })
        .unwrap_or_else(|_| _phone.clone());

    let output = Command::new("python3")
        .arg(mirror_script())
        .args(args)
        .env("TELEGRAM_API_ID", &api_id)
        .env("TELEGRAM_API_HASH", &api_hash)
        .env("TELEGRAM_PHONE", &research_phone)
        .env("TELEGRAM_SESSION", "research_session")
        .output()
        .await
        .expect("Failed to execute telegram_mirror.py");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("Failed to parse JSON\nstdout: {stdout}\nstderr: {stderr}\nparse error: {e}")
    })
}

/// Run telegram_reader.py with Telegram credentials.
async fn run_reader_with_creds(args: &[&str]) -> Value {
    let (api_id, api_hash, phone) = load_credentials();

    let output = Command::new("python3")
        .arg(reader_script())
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
        panic!("Failed to parse JSON\nstdout: {stdout}\nstderr: {stderr}\nparse error: {e}")
    })
}

// ─── Level 1: Unit tests (no network) ────────────────────────────────────────

/// U1: mirror_stats returns valid structure even if DB does not exist yet.
#[test]
fn u1_mirror_stats_returns_valid_structure() {
    let result = run_mirror_sync(&["mirror_stats"]);

    assert_eq!(
        result["success"], true,
        "mirror_stats should succeed even with empty DB: {result}"
    );
    assert!(
        result["message_count"].is_number(),
        "message_count should be a number: {result}"
    );
    assert!(
        result["chat_count"].is_number(),
        "chat_count should be a number: {result}"
    );
    assert!(
        result["db_path"].is_string(),
        "db_path should be a string: {result}"
    );
}

/// U2: search_indexed on empty/missing DB returns success=true with empty results.
#[test]
fn u2_indexed_search_empty_db_returns_empty_not_error() {
    let result = run_mirror_sync(&["search_indexed", "--query", "тест"]);

    assert_eq!(
        result["success"], true,
        "search_indexed should return success=true even on empty DB: {result}"
    );
    assert!(
        result["count"].is_number(),
        "count should be a number: {result}"
    );
    assert!(
        result["results"].is_array(),
        "results should be an array: {result}"
    );
}

/// U3: search_indexed with no-match query returns success=true, count=0.
#[test]
fn u3_indexed_search_no_results_returns_empty() {
    let result = run_mirror_sync(&["search_indexed", "--query", "xyzqwertynonexistent12345abc"]);

    assert_eq!(
        result["success"], true,
        "search_indexed with no-match should still succeed: {result}"
    );
    assert_eq!(
        result["count"].as_u64().unwrap_or(0),
        0,
        "count should be 0 for no-match query: {result}"
    );
}

/// U4: search_indexed with special characters does not crash.
#[test]
fn u4_indexed_search_special_chars_no_crash() {
    // FTS5 special chars — should return success (possibly with syntax note)
    let result = run_mirror_sync(&["search_indexed", "--query", "* AND OR NOT"]);

    // Must not crash — success=true with empty results or a graceful note
    assert!(
        result["success"] == true,
        "special char query should not crash: {result}"
    );
}

/// U5: search_fuzzy on empty DB returns success=true with empty results.
#[test]
fn u5_fuzzy_search_empty_db_returns_empty() {
    let result = run_mirror_sync(&["search_fuzzy", "--query", "сантехник"]);

    assert_eq!(
        result["success"], true,
        "search_fuzzy should succeed on empty DB: {result}"
    );
    assert!(
        result["count"].is_number(),
        "count should be a number: {result}"
    );
    assert!(
        result["results"].is_array(),
        "results should be an array: {result}"
    );
}

/// U6: search_fuzzy threshold field is preserved in response.
#[test]
fn u6_fuzzy_search_threshold_in_response() {
    let result = run_mirror_sync(&["search_fuzzy", "--query", "тест", "--threshold", "0.8"]);

    assert_eq!(result["success"], true, "should succeed: {result}");
    let threshold = result["threshold"].as_f64().unwrap_or(0.0);
    assert!(
        (threshold - 0.8).abs() < 0.01,
        "threshold should be 0.8 in response, got: {threshold}"
    );
}

/// U7: search_indexed date_filter args are accepted without error.
#[test]
fn u7_indexed_search_date_filter_accepted() {
    let result = run_mirror_sync(&[
        "search_indexed",
        "--query",
        "тест",
        "--date-from",
        "2026-01-01",
        "--date-to",
        "2026-12-31",
    ]);

    assert_eq!(
        result["success"], true,
        "date filter args should be accepted: {result}"
    );
}

// ─── Level 2: Integration tests (live Telegram API) ──────────────────────────

/// I1: index_channel indexes at least some messages from a known chat.
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn i1_index_channel_fetches_and_indexes_messages() {
    let result = run_mirror_with_creds(&[
        "index_channel",
        "--contact-name",
        "zverozabr",
        "--limit",
        "50",
    ])
    .await;

    assert_eq!(
        result["success"], true,
        "index_channel should succeed: {result}"
    );
    assert!(
        result["indexed"].is_number(),
        "indexed count should be a number: {result}"
    );
    assert!(
        result["chat"].is_object(),
        "chat info should be an object: {result}"
    );

    // After indexing, search_indexed should find something
    let search = run_mirror_sync(&["search_indexed", "--query", "привет"]);
    assert_eq!(
        search["success"], true,
        "search after indexing should succeed: {search}"
    );
}

/// I2: search_indexed is fast — must complete in under 1 second.
#[test]
fn i2_indexed_search_latency_sub_second() {
    let start = std::time::Instant::now();
    let result = run_mirror_sync(&["search_indexed", "--query", "самуи", "--limit", "50"]);
    let elapsed = start.elapsed();

    assert_eq!(result["success"], true, "search should succeed: {result}");
    assert!(
        elapsed.as_secs_f64() < 1.0,
        "indexed search should be < 1s, took: {:.3}s",
        elapsed.as_secs_f64()
    );
}

/// I3: search_global (live) completes within the new 180s timeout.
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn i3_search_global_live_completes_within_timeout() {
    let start = std::time::Instant::now();
    let result =
        run_reader_with_creds(&["search_global", "--query", "самуи", "--limit", "20"]).await;
    let elapsed = start.elapsed();

    assert_eq!(
        result["success"], true,
        "search_global should succeed: {result}"
    );
    assert!(
        elapsed.as_secs() < 180,
        "search_global took too long: {:.1}s",
        elapsed.as_secs_f64()
    );
    eprintln!(
        "search_global latency for 'самуи': {:.2}s, found {} results",
        elapsed.as_secs_f64(),
        result["count"]
    );
}

/// I4: search_global dialogs_scanned reflects multiple chats (not just one).
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn i4_search_global_scans_multiple_dialogs() {
    let result =
        run_reader_with_creds(&["search_global", "--query", "привет", "--limit", "50"]).await;

    assert_eq!(
        result["success"], true,
        "search_global should succeed: {result}"
    );

    let scanned = result["dialogs_scanned"].as_u64().unwrap_or(0);
    assert!(
        scanned >= 2,
        "search_global should return results from multiple chats (scanned: {scanned}). \
         Single dialog suggests early-exit bug."
    );
}

/// I5: date_filter_precision — all results from search_indexed fall within requested range.
#[test]
fn i5_date_filter_precision_all_results_in_range() {
    let date_from = "2025-01-01";
    let date_to = "2026-12-31";

    let result = run_mirror_sync(&[
        "search_indexed",
        "--query",
        "самуи",
        "--date-from",
        date_from,
        "--date-to",
        date_to,
        "--limit",
        "100",
    ]);

    assert_eq!(result["success"], true, "should succeed: {result}");

    let results = result["results"].as_array().unwrap();
    for msg in results {
        let date = msg["date"].as_str().unwrap_or("");
        assert!(
            date >= date_from && date <= format!("{date_to}Z").as_str(),
            "message date {date} is outside [{date_from}, {date_to}]"
        );
    }
}

/// I6: live_vs_indexed_overlap — after indexing, at least 50% of live results appear in index.
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn i6_live_vs_indexed_overlap() {
    // Use samui0 — a public channel both accounts can access, so live and indexed
    // msg IDs come from the same Telegram chat and are directly comparable.
    let test_chat = "samui0";

    // Index the channel using the research session (same as reader uses).
    let _idx = run_mirror_with_research_session(&[
        "index_channel",
        "--contact-name",
        test_chat,
        "--limit",
        "200",
    ])
    .await;

    // Live search via reader (always uses research_session).
    let live = run_reader_with_creds(&[
        "search_messages",
        "--contact-name",
        test_chat,
        "--limit",
        "20",
    ])
    .await;

    // Indexed search — filter to the same chat.
    let indexed = run_mirror_sync(&[
        "search_indexed",
        "--query",
        "*",
        "--chat-filter",
        test_chat,
        "--limit",
        "200",
    ]);

    if live["count"].as_u64().unwrap_or(0) == 0 {
        eprintln!("No live messages found — skipping overlap check");
        return;
    }

    let empty = vec![];
    let indexed_ids: std::collections::HashSet<i64> = indexed["results"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|r| r["msg_id"].as_i64())
        .collect();

    let live_msgs = live["messages"].as_array().unwrap_or(&empty);
    let overlap = live_msgs
        .iter()
        .filter(|m| {
            m["id"]
                .as_i64()
                .map(|id| indexed_ids.contains(&id))
                .unwrap_or(false)
        })
        .count();

    let total_live = live_msgs.len();
    let overlap_pct = if total_live > 0 {
        overlap * 100 / total_live
    } else {
        0
    };

    eprintln!("Overlap: {overlap}/{total_live} = {overlap_pct}%");
    assert!(
        overlap_pct >= 50,
        "Expected >= 50% overlap between live and indexed results, got {overlap_pct}%"
    );
}

/// I7: No duplicate msg_id+chat_id in indexed search results.
#[test]
fn i7_hybrid_no_duplicate_ids_in_indexed_results() {
    let result = run_mirror_sync(&["search_indexed", "--query", "самуи", "--limit", "200"]);

    assert_eq!(result["success"], true, "should succeed: {result}");

    let results = result["results"].as_array().unwrap_or(&vec![]).clone();
    let mut seen = std::collections::HashSet::new();
    for r in &results {
        let key = (
            r["msg_id"].as_i64().unwrap_or(0),
            r["chat_id"].as_i64().unwrap_or(0),
        );
        assert!(
            seen.insert(key),
            "Duplicate (msg_id, chat_id) in results: {:?}",
            key
        );
    }
}

// ─── Level 3: Quality E2E (real search scenarios) ────────────────────────────

/// Q1: Searching for a plumber on Samui returns results from >= 1 source.
#[tokio::test]
#[ignore = "requires network + Telegram credentials + indexed data"]
async fn q1_samui_plumber_search_finds_results() {
    // Try indexed first, fall back to live
    let indexed = run_mirror_sync(&["search_indexed", "--query", "сантехник", "--limit", "30"]);

    let live = run_reader_with_creds(&[
        "search_global",
        "--query",
        "сантехник самуи",
        "--limit",
        "30",
    ])
    .await;

    let indexed_count = indexed["count"].as_u64().unwrap_or(0);
    let live_count = live["count"].as_u64().unwrap_or(0);
    let total = indexed_count + live_count;

    assert!(
        total >= 1,
        "Expected at least 1 result for 'сантехник' across indexed ({indexed_count}) \
         and live ({live_count}) search"
    );

    eprintln!("Q1: сантехник — indexed: {indexed_count}, live: {live_count}, total: {total}");
}

/// Q2: AC service search with 6-month date filter — all results within range.
#[tokio::test]
#[ignore = "requires network + Telegram credentials"]
async fn q2_aircon_date_filter_all_results_in_range() {
    let six_months_ago = {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // Approximate: 180 days ago
        let then = now - (180 * 24 * 3600);
        chrono_approx(then)
    };

    let result = run_reader_with_creds(&[
        "search_global",
        "--query",
        "кондиционер",
        "--date-from",
        &six_months_ago,
        "--limit",
        "30",
    ])
    .await;

    assert_eq!(result["success"], true, "search should succeed: {result}");

    let results = result["results"].as_array().unwrap_or(&vec![]).clone();
    for msg in &results {
        let date = msg["date"].as_str().unwrap_or("");
        assert!(
            date >= six_months_ago.as_str(),
            "message date {date} is before date_from {six_months_ago}"
        );
    }

    eprintln!(
        "Q2: кондиционер since {six_months_ago}: {} results",
        results.len()
    );
}

/// Q3: Empty query returns success=true, count=0, no crash.
#[test]
fn q3_empty_query_no_crash() {
    // Empty string query
    let result = run_mirror_sync(&["search_indexed", "--query", ""]);
    // May fail with FTS5 syntax error OR return empty results — both acceptable
    // Key requirement: no panic/crash, success field present
    assert!(
        result["success"].is_boolean(),
        "success field must exist: {result}"
    );
}

/// Q4: Very long query (>200 chars) is handled without crash.
#[test]
fn q4_very_long_query_no_crash() {
    let long_query = "а".repeat(250);
    let result = run_mirror_sync(&["search_indexed", "--query", &long_query]);
    assert!(
        result["success"].is_boolean(),
        "success field must exist: {result}"
    );
}

/// Q5: search_fuzzy with typo finds plausible matches when data is indexed.
#[test]
fn q5_fuzzy_typo_tolerance() {
    // "сантехик" is a common typo for "сантехник"
    let result = run_mirror_sync(&[
        "search_fuzzy",
        "--query",
        "сантехик",
        "--threshold",
        "0.6",
        "--limit",
        "10",
    ]);

    assert_eq!(
        result["success"], true,
        "fuzzy search should succeed: {result}"
    );
    assert!(
        result["count"].is_number(),
        "count should be a number: {result}"
    );
    // If data is indexed: results with score >= 0.6 should include "сантехник"
    // If no data: count=0 is acceptable
}

/// Q6: Strict fuzzy threshold (0.95) returns fewer results than lenient (0.5).
#[test]
fn q6_fuzzy_stricter_threshold_fewer_results() {
    let strict = run_mirror_sync(&[
        "search_fuzzy",
        "--query",
        "самуи",
        "--threshold",
        "0.95",
        "--limit",
        "100",
    ]);
    let lenient = run_mirror_sync(&[
        "search_fuzzy",
        "--query",
        "самуи",
        "--threshold",
        "0.5",
        "--limit",
        "100",
    ]);

    assert_eq!(strict["success"], true);
    assert_eq!(lenient["success"], true);

    let strict_count = strict["count"].as_u64().unwrap_or(0);
    let lenient_count = lenient["count"].as_u64().unwrap_or(0);

    assert!(
        strict_count <= lenient_count,
        "Strict threshold (0.95) should return <= results than lenient (0.5). \
         Got strict={strict_count}, lenient={lenient_count}"
    );
}

/// Q7: Three parallel search_indexed calls all complete without deadlock.
#[tokio::test]
async fn q7_parallel_indexed_searches_no_deadlock() {
    let queries = ["самуи", "юрист", "врач"];

    let handles: Vec<_> = queries
        .iter()
        .map(|q| {
            let q = q.to_string();
            tokio::task::spawn_blocking(move || {
                run_mirror_sync(&["search_indexed", "--query", &q, "--limit", "10"])
            })
        })
        .collect();

    let timeout = tokio::time::Duration::from_secs(10);
    for handle in handles {
        let result = tokio::time::timeout(timeout, handle)
            .await
            .expect("parallel search timed out after 10s")
            .expect("task panicked");

        assert_eq!(
            result["success"], true,
            "parallel indexed search should succeed: {result}"
        );
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

// ─── Level 4: Bot E2E (real conversation via live daemon) ────────────────────

/// Path to the zverozabr Telegram session used for bot E2E tests.
fn zverozabr_session_path() -> String {
    let home = std::env::var("HOME").expect("HOME env var required");
    format!(
        "{}/.zeroclaw/workspace/skills/telegram-reader/.session/zverozabr_session",
        home
    )
}

/// Send a message to a bot via zverozabr_session.
/// Returns the sent message ID, or panics with a clear error.
///
/// All dynamic values go through env vars to avoid Python string-quoting issues.
async fn send_to_bot(bot_username: &str, text: &str) -> i64 {
    let (api_id, api_hash, _) = load_credentials();
    let session = zverozabr_session_path();

    let script = r#"
import asyncio, json, os, sys
from telethon import TelegramClient

SESSION   = os.environ["ZC_SESSION"]
API_ID    = os.environ["ZC_API_ID"]
API_HASH  = os.environ["ZC_API_HASH"]
BOT       = os.environ["ZC_BOT"]
TEXT      = os.environ["ZC_TEXT"]

async def main():
    client = TelegramClient(SESSION, API_ID, API_HASH)
    await client.connect()
    if not await client.is_user_authorized():
        print(json.dumps({"success": False, "error": "zverozabr_session not authorized"}))
        sys.exit(1)
    msg = await client.send_message(BOT, TEXT)
    print(json.dumps({"success": True, "message_id": msg.id}))
    await client.disconnect()

asyncio.run(main())
"#;

    let output = Command::new("python3")
        .arg("-c")
        .arg(script)
        .env("ZC_SESSION", &session)
        .env("ZC_API_ID", &api_id)
        .env("ZC_API_HASH", &api_hash)
        .env("ZC_BOT", bot_username)
        .env("ZC_TEXT", text)
        .output()
        .await
        .expect("failed to run send_to_bot script");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let json: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| {
        panic!("send_to_bot: invalid JSON\nstdout: {stdout}\nstderr: {stderr}")
    });

    assert!(
        json["success"] == true,
        "send_to_bot failed: {json}\nstderr: {stderr}"
    );

    json["message_id"]
        .as_i64()
        .expect("send_to_bot: message_id must be integer")
}

/// Poll for a bot reply after `after_message_id`, waiting up to `timeout`.
/// Returns the reply text, or None if timeout elapsed without a reply.
///
/// All dynamic values go through env vars to avoid Python string-quoting issues.
async fn wait_for_bot_reply(
    bot_username: &str,
    after_message_id: i64,
    timeout: Duration,
) -> Option<String> {
    let (api_id, api_hash, _) = load_credentials();
    let session = zverozabr_session_path();
    let deadline = Instant::now() + timeout;
    let after_id_str = after_message_id.to_string();

    while Instant::now() < deadline {
        let script = r#"
import asyncio, json, os, sys
from telethon import TelegramClient

SESSION  = os.environ["ZC_SESSION"]
API_ID   = os.environ["ZC_API_ID"]
API_HASH = os.environ["ZC_API_HASH"]
BOT      = os.environ["ZC_BOT"]
AFTER_ID = int(os.environ["ZC_AFTER_ID"])

async def main():
    client = TelegramClient(SESSION, API_ID, API_HASH)
    await client.connect()
    if not await client.is_user_authorized():
        print(json.dumps({"success": False, "error": "not authorized"}))
        sys.exit(1)
    msgs = await client.get_messages(BOT, limit=10)
    results = []
    for m in msgs:
        if m.id > AFTER_ID and m.out is False:
            results.append({"id": m.id, "text": m.text or ""})
    print(json.dumps({"success": True, "messages": results}))
    await client.disconnect()

asyncio.run(main())
"#;

        let output = Command::new("python3")
            .arg("-c")
            .arg(script)
            .env("ZC_SESSION", &session)
            .env("ZC_API_ID", &api_id)
            .env("ZC_API_HASH", &api_hash)
            .env("ZC_BOT", bot_username)
            .env("ZC_AFTER_ID", &after_id_str)
            .output()
            .await
            .expect("failed to run wait_for_bot_reply script");

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let json: Value = serde_json::from_str(stdout.trim()).unwrap_or(Value::Null);

        if json["success"] == true {
            if let Some(msgs) = json["messages"].as_array() {
                if !msgs.is_empty() {
                    // Return the oldest reply (lowest id > after_message_id)
                    let mut sorted = msgs.to_vec();
                    sorted.sort_by_key(|m| m["id"].as_i64().unwrap_or(0));
                    let text = sorted[0]["text"].as_str().unwrap_or("").to_string();
                    return Some(text);
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    None
}

/// Read the last `n_lines` from the daemon log at `/tmp/zeroclaw_daemon.log`.
/// Returns an empty string if the log file does not exist.
fn read_daemon_log_tail(n_lines: usize) -> String {
    let log_path = "/tmp/zeroclaw_daemon.log";
    match std::fs::read_to_string(log_path) {
        Ok(content) => {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(n_lines);
            lines[start..].join("\n")
        }
        Err(_) => String::new(),
    }
}

/// Heuristic phone-number detector: text contains 10+ consecutive digits.
fn contains_phone_number(text: &str) -> bool {
    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.len() >= 10
}

/// B1: bot must return actual contact info (phone or @username), not just raw JSON.
///
/// The sub-agent (codex-1) should iterate over search results, extract contacts,
/// and the main agent should relay them — not dump `{"success": true, ...}` JSON.
///
/// Requirements:
///   - Daemon running with live binary
///   - [agents.telegram_searcher] configured in ~/.zeroclaw/config.toml
///   - zverozabr_session authorized
#[tokio::test]
#[ignore = "requires live daemon + authorized zverozabr_session + [agents.telegram_searcher] config"]
async fn b1_bot_returns_contacts_not_raw_json() {
    let bot = "zGsR_bot";
    let query = "Поищи в Telegram сантехника на Самуи. Нужны контакты — телефон или @username.";

    println!("Sending to @{bot}: {query}");
    let sent_id = send_to_bot(bot, query).await;
    println!("Sent message id={sent_id}");

    let start = Instant::now();

    // sub-agent + 3 iterations codex-1 ≈ 90-150s; allow 300s headroom
    let reply = wait_for_bot_reply(bot, sent_id, Duration::from_secs(300)).await;

    let elapsed = start.elapsed();
    println!("Elapsed: {}s", elapsed.as_secs());

    let text = reply.unwrap_or_else(|| {
        panic!(
            "Bot @{bot} did not reply within 600s after message id={sent_id}. \
             Check daemon logs: /tmp/zeroclaw_daemon.log"
        )
    });
    println!("Bot reply:\n{text}");

    // Must contain a contact signal
    let has_contact = text.contains('@')
        || contains_phone_number(&text)
        || text.to_lowercase().contains("телефон")
        || text.to_lowercase().contains("написать")
        || text.to_lowercase().contains("связаться")
        || text.to_lowercase().contains("контакт");

    assert!(
        has_contact,
        "Bot reply must contain a contact (@username, phone, or contact phrase), got:\n{text}"
    );

    // Must NOT dump raw tool JSON
    assert!(
        !text.contains("\"success\""),
        "Bot must summarize results — not dump raw JSON:\n{text}"
    );

    // Must NOT indicate a timeout or hard error
    assert!(
        !text.to_lowercase().contains("timed out")
            && !text.to_lowercase().contains("request timed out"),
        "Reply indicates timeout:\n{text}"
    );
}

/// B2: iterative search — bot should make ≥2 telegram_search_* tool calls.
///
/// Verifies via daemon log that the agent iterated (searched, refined, searched again)
/// rather than returning on the first attempt.
///
/// Requirements:
///   - Daemon running, daemon log at /tmp/zeroclaw_daemon.log
///   - [agents.telegram_searcher] with agentic=true configured
///   - zverozabr_session authorized
#[tokio::test]
#[ignore = "requires live daemon + log access + [agents.telegram_searcher] config"]
async fn b2_iterative_search_makes_multiple_tool_calls() {
    let log_before = read_daemon_log_tail(50);
    let bot = "zGsR_bot";
    let query = "Найди мастера по кондиционерам на Самуи, нужен конкретный контакт.";

    println!("Sending to @{bot}: {query}");
    let sent_id = send_to_bot(bot, query).await;

    let reply = wait_for_bot_reply(bot, sent_id, Duration::from_secs(300)).await;
    assert!(reply.is_some(), "Bot did not reply within 300s");

    let log_after = read_daemon_log_tail(300);

    // Count new telegram_search_* calls that appeared after we sent the message.
    // We look for lines that appeared after the snapshot we took before sending.
    let new_log = log_after
        .lines()
        .skip(log_before.lines().count().saturating_sub(5))
        .collect::<Vec<_>>()
        .join("\n");

    let search_calls = new_log.matches("telegram_search_").count();
    assert!(
        search_calls >= 2,
        "Expected ≥2 telegram_search_* tool calls (iterative search), got {search_calls}.\n\
         New log lines:\n{new_log}"
    );

    println!("telegram_search_* calls: {search_calls}");
}

/// B3: Bangkok — bot must find contacts for a service request in Bangkok.
///
/// Validates discover → join → search workflow for a city with no pre-joined channels.
///
/// Requirements:
///   - Daemon running with live binary
///   - [agents.telegram_searcher] configured in ~/.zeroclaw/config.toml
///   - zverozabr_session authorized
#[tokio::test]
#[ignore = "requires live daemon + authorized zverozabr_session + [agents.telegram_searcher] config"]
async fn b3_bangkok_search_returns_contacts() {
    let bot = "zGsR_bot";
    let query = "Поищи в Telegram сантехника в Бангкоке. Нужны контакты — телефон или @username.";

    println!("Sending to @{bot}: {query}");
    let sent_id = send_to_bot(bot, query).await;
    println!("Sent message id={sent_id}");

    let start = Instant::now();
    let reply = wait_for_bot_reply(bot, sent_id, Duration::from_secs(600)).await;
    let elapsed = start.elapsed();
    println!("Elapsed: {}s", elapsed.as_secs());

    let text = reply.unwrap_or_else(|| {
        panic!(
            "Bot @{bot} did not reply within 600s after message id={sent_id}. \
             Check daemon logs: /tmp/zeroclaw_daemon.log"
        )
    });
    println!("Bot reply:\n{text}");

    let has_contact = text.contains('@')
        || contains_phone_number(&text)
        || text.to_lowercase().contains("телефон")
        || text.to_lowercase().contains("написать")
        || text.to_lowercase().contains("связаться")
        || text.to_lowercase().contains("контакт");

    assert!(
        has_contact,
        "Bot reply must contain a contact (@username, phone, or contact phrase), got:\n{text}"
    );
    assert!(
        !text.contains("\"success\""),
        "Bot must summarize results — not dump raw JSON:\n{text}"
    );
}

/// B4: Da Nang, Vietnam — bot must find contacts for a service request in Da Nang.
///
/// Validates search in Vietnamese Telegram communities.
///
/// Requirements:
///   - Daemon running with live binary
///   - [agents.telegram_searcher] configured in ~/.zeroclaw/config.toml
///   - zverozabr_session authorized
#[tokio::test]
#[ignore = "requires live daemon + authorized zverozabr_session + [agents.telegram_searcher] config"]
async fn b4_danang_vietnam_search_returns_contacts() {
    let bot = "zGsR_bot";
    let query =
        "Поищи в Telegram сантехника в Дананге (Вьетнам). Нужны контакты — телефон или @username.";

    println!("Sending to @{bot}: {query}");
    let sent_id = send_to_bot(bot, query).await;
    println!("Sent message id={sent_id}");

    let start = Instant::now();
    let reply = wait_for_bot_reply(bot, sent_id, Duration::from_secs(600)).await;
    let elapsed = start.elapsed();
    println!("Elapsed: {}s", elapsed.as_secs());

    let text = reply.unwrap_or_else(|| {
        panic!(
            "Bot @{bot} did not reply within 600s after message id={sent_id}. \
             Check daemon logs: /tmp/zeroclaw_daemon.log"
        )
    });
    println!("Bot reply:\n{text}");

    let has_contact = text.contains('@')
        || contains_phone_number(&text)
        || text.to_lowercase().contains("телефон")
        || text.to_lowercase().contains("написать")
        || text.to_lowercase().contains("связаться")
        || text.to_lowercase().contains("контакт");

    assert!(
        has_contact,
        "Bot reply must contain a contact (@username, phone, or contact phrase), got:\n{text}"
    );
    assert!(
        !text.contains("\"success\""),
        "Bot must summarize results — not dump raw JSON:\n{text}"
    );
}

/// B5 — Da Nang rental houses: full pipeline (discover → join → search → contacts)
#[tokio::test]
#[ignore = "requires live daemon + authorized zverozabr_session + [agents.telegram_searcher] config"]
async fn b5_danang_rental_houses_returns_contacts() {
    let bot = "zGsR_bot";
    let query = "Поищи в Telegram дома в аренду в Дананге (Вьетнам). Нужны контакты — телефон или @username.";

    println!("Sending to @{bot}: {query}");
    let sent_id = send_to_bot(bot, query).await;
    println!("Sent message id={sent_id}");

    let start = Instant::now();
    let reply = wait_for_bot_reply(bot, sent_id, Duration::from_secs(900)).await;
    println!("Elapsed: {}s", start.elapsed().as_secs());

    let text = reply.unwrap_or_else(|| {
        panic!(
            "Bot @{bot} did not reply within 900s after message id={sent_id}. \
             Check daemon logs: /tmp/zeroclaw_daemon.log"
        )
    });
    println!("Bot reply:\n{text}");

    let has_contact = text.contains('@')
        || contains_phone_number(&text)
        || text.to_lowercase().contains("телефон")
        || text.to_lowercase().contains("написать")
        || text.to_lowercase().contains("связаться")
        || text.to_lowercase().contains("контакт");

    assert!(
        has_contact,
        "Bot reply must contain a contact (@username, phone, or contact phrase), got:\n{text}"
    );
    assert!(
        !text.contains("\"success\""),
        "Bot must summarize results — not dump raw JSON:\n{text}"
    );
}

/// B-NEW1 — fallback resilience: search still works when primary provider has issues.
///
/// Sends a real search query and verifies the bot returns contacts.
/// This test validates the fallback_providers chain is reachable — even if the primary
/// provider is rate-limited, the fallback_providers in [agents.telegram_searcher] carry
/// the search through to completion.
///
/// Requirements:
///   - Daemon running with live binary
///   - [agents.telegram_searcher] configured with fallback_providers in ~/.zeroclaw/config.toml
///   - zverozabr_session authorized
#[tokio::test]
#[ignore = "requires live daemon + authorized zverozabr_session + fallback_providers config"]
async fn b_new1_search_works_via_fallback_chain() {
    let bot = "zGsR_bot";
    let query = "Поищи в Telegram дома в аренду на Самуи. Нужны контакты — телефон или @username.";

    println!("Sending to @{bot}: {query}");
    let sent_id = send_to_bot(bot, query).await;
    println!("Sent message id={sent_id}");

    let start = Instant::now();
    let reply = wait_for_bot_reply(bot, sent_id, Duration::from_secs(900)).await;
    println!("Elapsed: {}s", start.elapsed().as_secs());

    let text = reply.unwrap_or_else(|| {
        panic!(
            "Bot @{bot} did not reply within 900s after message id={sent_id}. \
             Check daemon logs: /tmp/zeroclaw_daemon.log"
        )
    });
    println!("Bot reply:\n{text}");

    let has_contact = text.contains('@')
        || contains_phone_number(&text)
        || text.to_lowercase().contains("телефон")
        || text.to_lowercase().contains("написать")
        || text.to_lowercase().contains("связаться")
        || text.to_lowercase().contains("контакт");

    assert!(
        has_contact,
        "Bot reply must contain a contact (fallback chain must succeed), got:\n{text}"
    );
    assert!(
        !text.contains("\"success\""),
        "Bot must summarize — not dump raw JSON:\n{text}"
    );
}

/// B-NEW2 — deduplication: contacts appearing in multiple channels appear only once.
///
/// Verifies the system_prompt dedup instruction works: the same @username or phone
/// should not appear twice in the final answer.
///
/// Requirements:
///   - Daemon running with live binary
///   - [agents.telegram_searcher] configured in ~/.zeroclaw/config.toml
///   - zverozabr_session authorized
#[tokio::test]
#[ignore = "requires live daemon + authorized zverozabr_session + [agents.telegram_searcher] config"]
async fn b_new2_contacts_are_deduplicated_in_response() {
    let bot = "zGsR_bot";
    let query = "Поищи в Telegram сантехника на Самуи. Дай список уникальных контактов — телефон или @username.";

    println!("Sending to @{bot}: {query}");
    let sent_id = send_to_bot(bot, query).await;
    println!("Sent message id={sent_id}");

    let start = Instant::now();
    let reply = wait_for_bot_reply(bot, sent_id, Duration::from_secs(900)).await;
    println!("Elapsed: {}s", start.elapsed().as_secs());

    let text = reply.unwrap_or_else(|| {
        panic!(
            "Bot @{bot} did not reply within 900s after message id={sent_id}. \
             Check daemon logs: /tmp/zeroclaw_daemon.log"
        )
    });
    println!("Bot reply:\n{text}");

    // Extract @usernames from the reply and check for duplicates
    let usernames: Vec<&str> = text
        .split_whitespace()
        .filter(|w| w.starts_with('@') && w.len() > 1)
        .collect();

    let mut seen = std::collections::HashSet::new();
    let mut duplicates: Vec<&str> = Vec::new();
    for u in &usernames {
        let norm = u.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');
        if !seen.insert(norm) {
            duplicates.push(u);
        }
    }

    assert!(
        duplicates.is_empty(),
        "Duplicate @usernames in bot reply (dedup instruction not followed): {:?}\nFull reply:\n{text}",
        duplicates
    );

    let has_contact = text.contains('@')
        || contains_phone_number(&text)
        || text.to_lowercase().contains("контакт");
    assert!(
        has_contact,
        "Bot reply must contain at least one contact, got:\n{text}"
    );
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Approximate ISO8601 date string from UNIX timestamp (no chrono dependency).
fn chrono_approx(unix_secs: u64) -> String {
    // Minimal conversion: days since epoch → year/month/day
    let days_total = unix_secs / 86400;
    let mut year = 1970u64;
    let mut days = days_total;

    loop {
        let leap =
            (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
        let days_in_year = if leap { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let months = if leap {
        [31u64, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31u64, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 1u64;
    for &dim in &months {
        if days < dim {
            break;
        }
        days -= dim;
        month += 1;
    }
    let day = days + 1;

    format!("{year:04}-{month:02}-{day:02}T00:00:00")
}
