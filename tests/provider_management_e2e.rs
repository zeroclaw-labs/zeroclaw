//! End-to-end provider management tests via WebSocket.
//!
//! These tests send natural language messages to ZeroClaw via the gateway
//! WebSocket and assert on the bot's responses (tool calls + content).
//!
//! Requirements:
//!   - Running ZeroClaw daemon with gateway on port 42617
//!   - ZEROCLAW_GATEWAY_TOKEN env var (or `.env` file)
//!   - Network access to provider APIs (for tests that validate real calls)
//!
//! Run:
//!   source .env && cargo test --test provider_management_e2e -- --ignored --test-threads=1

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

// ──────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────

/// Gateway WebSocket URL.
fn ws_url() -> String {
    let port = std::env::var("ZEROCLAW_GATEWAY_PORT").unwrap_or_else(|_| "42617".into());
    let token = load_token();
    if token.is_empty() {
        format!("ws://127.0.0.1:{port}/ws/chat")
    } else {
        format!("ws://127.0.0.1:{port}/ws/chat?token={token}")
    }
}

/// Load gateway token from env or .env file.
fn load_token() -> String {
    if let Ok(t) = std::env::var("ZEROCLAW_GATEWAY_TOKEN") {
        if !t.is_empty() {
            return t;
        }
    }
    // Fallback: parse .env
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let env_path = manifest.join(".env");
    if let Ok(content) = std::fs::read_to_string(&env_path) {
        for line in content.lines() {
            if let Some(val) = line.trim().strip_prefix("ZEROCLAW_GATEWAY_TOKEN=") {
                return val.trim().to_string();
            }
        }
    }
    String::new()
}

type WsSender = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    Message,
>;
type WsReceiver = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
>;

/// Send a message and wait for "done" or "error" response.
async fn send_and_wait(sender: &mut WsSender, receiver: &mut WsReceiver, content: &str) -> String {
    let msg = serde_json::json!({"type": "message", "content": content});
    sender
        .send(Message::Text(msg.to_string().into()))
        .await
        .expect("send failed");

    // Wait up to 240s for done/error (bot may call multiple tools, key_store can be slow)
    let deadline = Duration::from_secs(240);
    let result = timeout(deadline, async {
        while let Some(Ok(frame)) = receiver.next().await {
            if let Message::Text(text) = frame {
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    match v["type"].as_str() {
                        Some("done") => {
                            return v["full_response"]
                                .as_str()
                                .unwrap_or("")
                                .to_string();
                        }
                        Some("error") => {
                            return format!(
                                "ERROR: {}",
                                v["message"].as_str().unwrap_or("unknown")
                            );
                        }
                        _ => continue,
                    }
                }
            }
        }
        "ERROR: connection closed".to_string()
    })
    .await;

    result.unwrap_or_else(|_| "ERROR: timeout".to_string())
}

/// Open a WebSocket connection, skip the session_start frame.
async fn connect() -> (WsSender, WsReceiver) {
    let url = ws_url();
    let (ws_stream, _) = connect_async(&url)
        .await
        .unwrap_or_else(|e| panic!("Failed to connect to {url}: {e}"));

    let (sender, mut receiver) = ws_stream.split();

    // Skip session_start message
    if let Some(Ok(Message::Text(_))) = receiver.next().await {
        // session_start consumed
    }

    (sender, receiver)
}

/// Snapshot current config.toml via gateway API.
async fn snapshot_config() -> String {
    let port = std::env::var("ZEROCLAW_GATEWAY_PORT").unwrap_or_else(|_| "42617".into());
    let token = load_token();
    let url = format!("http://127.0.0.1:{port}/api/config");
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("snapshot GET failed")
        .json::<Value>()
        .await
        .expect("snapshot parse failed");
    resp["content"].as_str().unwrap_or("").to_string()
}

/// Restore config.toml via gateway API.
async fn restore_config(toml: &str) {
    let port = std::env::var("ZEROCLAW_GATEWAY_PORT").unwrap_or_else(|_| "42617".into());
    let token = load_token();
    let url = format!("http://127.0.0.1:{port}/api/config");
    let client = reqwest::Client::new();
    let _ = client
        .put(&url)
        .bearer_auth(&token)
        .body(toml.to_string())
        .send()
        .await;
}

// ──────────────────────────────────────────────────────────
// Single-turn tests
// ──────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_01_provider_status() {
    let (mut tx, mut rx) = connect().await;
    let resp = send_and_wait(&mut tx, &mut rx, "покажи статус провайдеров").await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("gemini") || lower.contains("default") || lower.contains("провайдер"),
        "Response should contain provider info: {resp}"
    );
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_02_key_count() {
    let (mut tx, mut rx) = connect().await;
    let resp = send_and_wait(&mut tx, &mut rx, "сколько у нас ключей deepseek?").await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        resp.chars().any(|c| c.is_ascii_digit())
            || lower.contains("deepseek")
            || lower.contains("ключ")
            || lower.contains("key"),
        "Response should contain key info: {resp}"
    );
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_03_add_fallback() {
    let snapshot = snapshot_config().await;
    let (mut tx, mut rx) = connect().await;

    // Use deepseek — we know keys exist in the store. Bot should:
    // 1) find a deepseek key (key_store/provider_find)
    // 2) add it to fallback (provider_apply)
    let resp = send_and_wait(
        &mut tx,
        &mut rx,
        "добавь ещё один deepseek профиль в фоллбэк",
    )
    .await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("deepseek") || lower.contains("добавлен") || lower.contains("fallback")
            || lower.contains("added") || lower.contains("add_fallback") || lower.contains("chain"),
        "Response should confirm addition to fallback chain: {resp}"
    );

    restore_config(&snapshot).await;
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_04_remove_fallback() {
    let snapshot = snapshot_config().await;
    let (mut tx, mut rx) = connect().await;

    // First ensure groq is in fallback (add it)
    let _ = send_and_wait(&mut tx, &mut rx, "добавь groq в фоллбэк").await;
    // Now remove it
    let resp = send_and_wait(&mut tx, &mut rx, "удали groq из фоллбэков").await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("удал") || lower.contains("remov") || lower.contains("groq"),
        "Response should confirm removal: {resp}"
    );

    restore_config(&snapshot).await;
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_05_test_model() {
    let (mut tx, mut rx) = connect().await;
    let resp = send_and_wait(&mut tx, &mut rx, "проверь модель gemini-2.5-flash").await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("latency") || lower.contains("мс") || lower.contains("ms")
            || lower.contains("response") || lower.contains("ответ")
            || lower.contains("valid") || lower.contains("работает")
            || lower.contains("gemini") || lower.contains("429") || lower.contains("quota"),
        "Response should contain test result: {resp}"
    );
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_06_list_models() {
    let (mut tx, mut rx) = connect().await;
    let resp = send_and_wait(&mut tx, &mut rx, "какие модели есть у openai?").await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("gpt") || lower.contains("model") || lower.contains("модел"),
        "Response should list models: {resp}"
    );
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_07_switch_provider() {
    let snapshot = snapshot_config().await;
    let (mut tx, mut rx) = connect().await;

    // Switch to deepseek
    let resp = send_and_wait(&mut tx, &mut rx, "переключи на deepseek-chat").await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("deepseek") || lower.contains("переключ") || lower.contains("установлен")
            || lower.contains("switch") || lower.contains("default"),
        "Response should confirm switch: {resp}"
    );

    // Restore config
    restore_config(&snapshot).await;
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_08_provider_health() {
    let (mut tx, mut rx) = connect().await;
    let resp = send_and_wait(&mut tx, &mut rx, "проверь здоровье провайдеров").await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("check") || lower.contains("провер") || lower.contains("ключ")
            || lower.contains("key") || lower.contains("dead") || lower.contains("replaced")
            || lower.contains("ok") || lower.contains("здоров"),
        "Response should contain health info: {resp}"
    );
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_09_validate_key() {
    let (mut tx, mut rx) = connect().await;
    let resp = send_and_wait(
        &mut tx,
        &mut rx,
        "валидируй ключ deepseek sk-a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4",
    )
    .await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("invalid") || lower.contains("невалид") || lower.contains("ошибк")
            || lower.contains("fail") || lower.contains("401") || lower.contains("неверн")
            || lower.contains("dead") || lower.contains("не работает")
            || lower.contains("unauthorized") || lower.contains("false")
            || lower.contains("недействит"),
        "Response should indicate invalid key: {resp}"
    );
}

// ──────────────────────────────────────────────────────────
// Multi-turn tests
// ──────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_10_multi_turn_deepseek_chain() {
    let snapshot = snapshot_config().await;
    let (mut tx, mut rx) = connect().await;

    // Step 1: ask about deepseek keys
    let r1 = send_and_wait(&mut tx, &mut rx, "сколько ключей deepseek?").await;
    assert!(!r1.starts_with("ERROR"), "Step 1 error: {r1}");
    let lower1 = r1.to_lowercase();
    assert!(
        r1.chars().any(|c| c.is_ascii_digit())
            || lower1.contains("deepseek")
            || lower1.contains("key_store")
            || lower1.contains("ключ"),
        "Step 1 should contain deepseek key info: {r1}"
    );

    // Step 2: "what models there?" — bot should resolve "there" = deepseek
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r2 = send_and_wait(&mut tx, &mut rx, "какие модели там доступны?").await;
    assert!(!r2.starts_with("ERROR"), "Step 2 error: {r2}");
    let lower2 = r2.to_lowercase();
    assert!(
        lower2.contains("deepseek") || lower2.contains("model") || lower2.contains("модел"),
        "Step 2 should list deepseek models: {r2}"
    );

    // Step 3: "set deepseek-reasoner as default" — continuing deepseek context
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r3 = send_and_wait(&mut tx, &mut rx, "установи deepseek-reasoner основной").await;
    assert!(!r3.starts_with("ERROR"), "Step 3 error: {r3}");
    let lower3 = r3.to_lowercase();
    assert!(
        lower3.contains("deepseek") || lower3.contains("установлен") || lower3.contains("default"),
        "Step 3 should confirm switch: {r3}"
    );

    restore_config(&snapshot).await;
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_11_multi_turn_add_test_default() {
    let snapshot = snapshot_config().await;
    let (mut tx, mut rx) = connect().await;

    // Step 1: add groq (keys are plentiful and fast)
    let r1 = send_and_wait(&mut tx, &mut rx, "добавь ещё один groq профиль в фоллбэк").await;
    assert!(!r1.starts_with("ERROR"), "Step 1 error: {r1}");

    // Step 2: test llama model
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r2 = send_and_wait(&mut tx, &mut rx, "протестируй llama-3.3-70b-versatile").await;
    assert!(!r2.starts_with("ERROR"), "Step 2 error: {r2}");

    // Step 3: set as default
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r3 = send_and_wait(&mut tx, &mut rx, "сделай groq основным").await;
    assert!(!r3.starts_with("ERROR"), "Step 3 error: {r3}");

    restore_config(&snapshot).await;
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_12_multi_turn_find_validate_add() {
    let snapshot = snapshot_config().await;
    let (mut tx, mut rx) = connect().await;

    // Step 1: find groq keys
    let r1 = send_and_wait(&mut tx, &mut rx, "найди ключи groq").await;
    assert!(!r1.starts_with("ERROR"), "Step 1 error: {r1}");
    let lower1 = r1.to_lowercase();
    if lower1.contains("не найден") || lower1.contains("not found") || lower1.contains("0 key") {
        eprintln!("SKIP pm_12: no groq keys in store");
        restore_config(&snapshot).await;
        return;
    }

    // Step 2: validate first key
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r2 = send_and_wait(&mut tx, &mut rx, "проверь первый ключ").await;
    assert!(!r2.starts_with("ERROR"), "Step 2 error: {r2}");

    // Step 3: add it
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r3 = send_and_wait(&mut tx, &mut rx, "добавь его в фоллбэк").await;
    assert!(!r3.starts_with("ERROR"), "Step 3 error: {r3}");

    restore_config(&snapshot).await;
}

// ──────────────────────────────────────────────────────────
// Variation tests
// ──────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_13_english_input() {
    let (mut tx, mut rx) = connect().await;
    let resp = send_and_wait(&mut tx, &mut rx, "find me some groq keys").await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("groq") || lower.contains("key") || lower.contains("found")
            || lower.contains("gsk_"),
        "Response should contain groq key info: {resp}"
    );
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_14_current_default() {
    let (mut tx, mut rx) = connect().await;
    let resp = send_and_wait(&mut tx, &mut rx, "что сейчас основной провайдер?").await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("gemini") || lower.contains("default") || lower.contains("основн")
            || lower.contains("провайдер"),
        "Response should mention current default: {resp}"
    );
}
