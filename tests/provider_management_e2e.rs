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

/// Load gateway token from env, .env file, or daemon-written gateway_token file.
fn load_token() -> String {
    // 1. Env var (highest priority)
    if let Ok(t) = std::env::var("ZEROCLAW_GATEWAY_TOKEN") {
        if !t.is_empty() {
            return t;
        }
    }
    // 2. .env file
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let env_path = manifest.join(".env");
    if let Ok(content) = std::fs::read_to_string(&env_path) {
        for line in content.lines() {
            if let Some(val) = line.trim().strip_prefix("ZEROCLAW_GATEWAY_TOKEN=") {
                let val = val.trim();
                if !val.is_empty() {
                    return val.to_string();
                }
            }
        }
    }
    // 3. Gateway token file (written by daemon for trusted skills)
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        let token_path = home.join(".zeroclaw/gateway_token");
        if let Ok(t) = std::fs::read_to_string(&token_path) {
            let t = t.trim().to_string();
            if !t.is_empty() {
                return t;
            }
        }
    }
    String::new()
}

type WsSender = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;
type WsReceiver = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
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
                            return v["full_response"].as_str().unwrap_or("").to_string();
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
/// Includes a cooldown to let the daemon drain background tasks between tests.
async fn connect() -> (WsSender, WsReceiver) {
    tokio::time::sleep(Duration::from_secs(3)).await;
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

/// Switch daemon to a different provider/model via config API.
async fn switch_provider_config(provider: &str, model: &str) {
    let snapshot = snapshot_config().await;
    let mut modified = snapshot.clone();
    if let Some(start) = modified.find("default_provider = \"") {
        if let Some(end) = modified[start..].find('\n') {
            modified.replace_range(
                start..start + end,
                &format!("default_provider = \"{provider}\""),
            );
        }
    }
    if let Some(start) = modified.find("default_model = \"") {
        if let Some(end) = modified[start..].find('\n') {
            modified.replace_range(start..start + end, &format!("default_model = \"{model}\""));
        }
    }
    restore_config(&modified).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
}

/// File to persist original provider config across test suite.
fn original_config_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/.e2e_original_config.toml")
}

/// Switch to OpenAI gpt-4o-mini if E2E_PROVIDER=openai is set.
///
/// Usage: E2E_PROVIDER=openai cargo test --test provider_management_e2e -- --ignored --test-threads=1
///
/// Falls back to default provider (gemini) when env var is not set.
#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_00_setup_provider() {
    let original = snapshot_config().await;
    std::fs::write(original_config_path(), &original).expect("save original config");

    if let Ok(provider) = std::env::var("E2E_PROVIDER") {
        let (prov, model) = match provider.as_str() {
            "openai" | "codex" => ("openai", "gpt-4o-mini"),
            "groq" => ("groq", "meta-llama/llama-4-scout-17b-16e-instruct"),
            other => {
                eprintln!("pm_00: unknown E2E_PROVIDER={other}, keeping default");
                return;
            }
        };
        switch_provider_config(prov, model).await;
        // Verify the switch works
        let (mut tx, mut rx) = connect().await;
        let resp = send_and_wait(&mut tx, &mut rx, "скажи одно слово: привет").await;
        if resp.starts_with("ERROR") {
            eprintln!("pm_00: {prov}/{model} failed: {resp}");
            eprintln!("pm_00: restoring default provider");
            restore_config(&original).await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        } else {
            eprintln!("pm_00: switched to {prov}/{model}");
        }
    } else {
        eprintln!("pm_00: E2E_PROVIDER not set, using default");
    }
}

/// Restore original provider config (runs last, alphabetically).
#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_99_restore_provider() {
    let path = original_config_path();
    if path.exists() {
        let original = std::fs::read_to_string(&path).expect("read original config");
        restore_config(&original).await;
        let _ = std::fs::remove_file(&path);
        eprintln!("pm_99: restored original provider config");
    }
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
        lower.contains("gemini")
            || lower.contains("default")
            || lower.contains("провайдер")
            || lower.contains("provider_status")
            || lower.contains("provider"),
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
        lower.contains("deepseek")
            || lower.contains("добавлен")
            || lower.contains("fallback")
            || lower.contains("added")
            || lower.contains("add_fallback")
            || lower.contains("chain"),
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
        lower.contains("latency")
            || lower.contains("мс")
            || lower.contains("ms")
            || lower.contains("response")
            || lower.contains("ответ")
            || lower.contains("valid")
            || lower.contains("работает")
            || lower.contains("gemini")
            || lower.contains("429")
            || lower.contains("quota"),
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
        lower.contains("deepseek")
            || lower.contains("переключ")
            || lower.contains("установлен")
            || lower.contains("switch")
            || lower.contains("default"),
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
        lower.contains("check")
            || lower.contains("провер")
            || lower.contains("ключ")
            || lower.contains("key")
            || lower.contains("dead")
            || lower.contains("replaced")
            || lower.contains("ok")
            || lower.contains("здоров")
            || lower.contains("health")
            || lower.contains("provider"),
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
        lower.contains("invalid")
            || lower.contains("невалид")
            || lower.contains("ошибк")
            || lower.contains("fail")
            || lower.contains("401")
            || lower.contains("неверн")
            || lower.contains("dead")
            || lower.contains("не работает")
            || lower.contains("unauthorized")
            || lower.contains("false")
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
    if lower1.contains("не найден") || lower1.contains("not found") || lower1.contains("0 key")
    {
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
        lower.contains("groq")
            || lower.contains("key")
            || lower.contains("found")
            || lower.contains("gsk_"),
        "Response should contain groq key info: {resp}"
    );
}

/// Test key validation with a known-good key — bot should call provider_test and confirm it works.
#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_15_validate_good_key() {
    // Get a working gemini key from the config fallback chain
    let config = snapshot_config().await;
    let key = config
        .lines()
        .find(|l| l.contains("gemini-found-1") && l.contains("AIza"))
        .and_then(|l| l.split('"').nth(3))
        .unwrap_or("");

    if key.is_empty() {
        eprintln!("SKIP pm_15: no gemini key in fallback config");
        return;
    }

    let (mut tx, mut rx) = connect().await;
    let msg = format!("провалидируй этот ключ gemini: {key}");
    let resp = send_and_wait(&mut tx, &mut rx, &msg).await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("valid")
            || lower.contains("работает")
            || lower.contains("валид")
            || lower.contains("latency")
            || lower.contains("ms")
            || lower.contains("мс")
            || lower.contains("ok")
            || lower.contains("успеш"),
        "Response should confirm key is valid: {resp}"
    );
}

/// Test key validation with a bad key — bot should report it's invalid.
#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_16_validate_bad_key() {
    let (mut tx, mut rx) = connect().await;
    let resp = send_and_wait(
        &mut tx,
        &mut rx,
        "провалидируй этот ключ gemini: AIzaSyBADKEY000000000000000000000000000",
    )
    .await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("invalid")
            || lower.contains("невалид")
            || lower.contains("ошибк")
            || lower.contains("не работает")
            || lower.contains("not valid")
            || lower.contains("fail")
            || lower.contains("401")
            || lower.contains("400")
            || lower.contains("false")
            || lower.contains("недействит")
            || lower.contains("не прошёл")
            || lower.contains("не прошел"),
        "Response should indicate invalid key: {resp}"
    );
}

/// Test that bot can validate multiple keys in sequence (multi-turn).
#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_17_validate_keys_multi_turn() {
    // Get a working gemini key from config
    let config = snapshot_config().await;
    let good_key = config
        .lines()
        .find(|l| l.contains("gemini-found-1") && l.contains("AIza"))
        .and_then(|l| l.split('"').nth(3))
        .unwrap_or("");
    if good_key.is_empty() {
        eprintln!("SKIP pm_17: no gemini key in fallback config");
        return;
    }

    let (mut tx, mut rx) = connect().await;

    // Step 1: validate a bad key
    let r1 = send_and_wait(
        &mut tx,
        &mut rx,
        "проверь этот ключ deepseek: sk-a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4",
    )
    .await;
    assert!(!r1.starts_with("ERROR"), "Step 1 error: {r1}");

    // Step 2: validate a known-good gemini key
    tokio::time::sleep(Duration::from_secs(2)).await;
    let msg = format!("а теперь проверь ключ gemini {good_key}");
    let r2 = send_and_wait(&mut tx, &mut rx, &msg).await;
    assert!(!r2.starts_with("ERROR"), "Step 2 error: {r2}");
    let lower2 = r2.to_lowercase();
    assert!(
        lower2.contains("valid")
            || lower2.contains("работает")
            || lower2.contains("валид")
            || lower2.contains("действит")
            || lower2.contains("ok")
            || lower2.contains("latency")
            || lower2.contains("ms")
            || lower2.contains("успеш"),
        "Step 2 should confirm the key works: {r2}"
    );
}

/// MiniMax full flow: find key → validate → add to fallback → switch model → clean up.
#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_18_minimax_validate_and_switch() {
    let snapshot = snapshot_config().await;
    let (mut tx, mut rx) = connect().await;

    // Step 1: find a minimax key
    let r1 = send_and_wait(&mut tx, &mut rx, "найди рабочий ключ minimax").await;
    assert!(!r1.starts_with("ERROR"), "Step 1 error: {r1}");
    let lower1 = r1.to_lowercase();
    if lower1.contains("не найден") || lower1.contains("not found") || lower1.contains("0 key")
    {
        eprintln!("SKIP pm_18: no minimax keys in store");
        restore_config(&snapshot).await;
        return;
    }

    // Step 2: add minimax to fallback with the found key, then test it
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r2 = send_and_wait(
        &mut tx,
        &mut rx,
        "добавь этот ключ minimax в фоллбэк и протестируй модель MiniMax-Text-01",
    )
    .await;
    assert!(!r2.starts_with("ERROR"), "Step 2 error: {r2}");
    let lower2 = r2.to_lowercase();
    assert!(
        lower2.contains("minimax")
            || lower2.contains("добавлен")
            || lower2.contains("fallback")
            || lower2.contains("test")
            || lower2.contains("latency")
            || lower2.contains("ms")
            || lower2.contains("valid")
            || lower2.contains("ok")
            || lower2.contains("added"),
        "Step 2 should confirm add+test: {r2}"
    );

    // Step 3: switch default to minimax
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r3 = send_and_wait(
        &mut tx,
        &mut rx,
        "переключи основного провайдера на minimax MiniMax-Text-01",
    )
    .await;
    assert!(!r3.starts_with("ERROR"), "Step 3 error: {r3}");
    let lower3 = r3.to_lowercase();
    assert!(
        lower3.contains("minimax")
            || lower3.contains("переключ")
            || lower3.contains("установлен")
            || lower3.contains("default")
            || lower3.contains("switch"),
        "Step 3 should confirm model switch: {r3}"
    );

    restore_config(&snapshot).await;
}

/// DeepSeek: find key → validate → add fallback → switch model.
#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_19_deepseek_full_flow() {
    let snapshot = snapshot_config().await;
    let (mut tx, mut rx) = connect().await;

    // Step 1: find a deepseek key and add to fallback
    let r1 = send_and_wait(
        &mut tx,
        &mut rx,
        "найди рабочий ключ deepseek, добавь в фоллбэк и протестируй модель deepseek-chat",
    )
    .await;
    assert!(!r1.starts_with("ERROR"), "Step 1 error: {r1}");
    let lower1 = r1.to_lowercase();
    if lower1.contains("не найден") || lower1.contains("not found") || lower1.contains("0 key")
    {
        eprintln!("SKIP pm_19: no deepseek keys in store");
        restore_config(&snapshot).await;
        return;
    }

    // Step 2: switch default to deepseek
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r2 = send_and_wait(
        &mut tx,
        &mut rx,
        "переключи основного провайдера на deepseek deepseek-chat",
    )
    .await;
    assert!(!r2.starts_with("ERROR"), "Step 2 error: {r2}");
    let lower2 = r2.to_lowercase();
    assert!(
        lower2.contains("deepseek")
            || lower2.contains("переключ")
            || lower2.contains("установлен")
            || lower2.contains("default")
            || lower2.contains("switch")
            || lower2.contains("set_default"),
        "Step 2 should confirm switch to deepseek: {r2}"
    );

    restore_config(&snapshot).await;
}

/// Moonshot: find key → validate → add fallback → switch model.
#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_20_moonshot_full_flow() {
    let snapshot = snapshot_config().await;
    let (mut tx, mut rx) = connect().await;

    // Step 1: find a moonshot key and add to fallback
    let r1 = send_and_wait(
        &mut tx,
        &mut rx,
        "найди рабочий ключ moonshot, добавь в фоллбэк и протестируй moonshot-v1-8k",
    )
    .await;
    assert!(!r1.starts_with("ERROR"), "Step 1 error: {r1}");
    let lower1 = r1.to_lowercase();
    if lower1.contains("не найден") || lower1.contains("not found") || lower1.contains("0 key")
    {
        eprintln!("SKIP pm_20: no moonshot keys in store");
        restore_config(&snapshot).await;
        return;
    }

    // Step 2: switch default to moonshot
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r2 = send_and_wait(
        &mut tx,
        &mut rx,
        "переключи основного провайдера на moonshot moonshot-v1-8k",
    )
    .await;
    assert!(!r2.starts_with("ERROR"), "Step 2 error: {r2}");
    let lower2 = r2.to_lowercase();
    assert!(
        lower2.contains("moonshot")
            || lower2.contains("переключ")
            || lower2.contains("установлен")
            || lower2.contains("default")
            || lower2.contains("switch")
            || lower2.contains("set_default"),
        "Step 2 should confirm switch to moonshot: {r2}"
    );

    restore_config(&snapshot).await;
}

/// Mistral: find key → validate → add fallback → switch model.
#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_21_mistral_full_flow() {
    let snapshot = snapshot_config().await;
    let (mut tx, mut rx) = connect().await;

    // Step 1: find a mistral key and add to fallback
    let r1 = send_and_wait(
        &mut tx,
        &mut rx,
        "найди рабочий ключ mistral, добавь в фоллбэк и протестируй модель mistral-large-latest",
    )
    .await;
    assert!(!r1.starts_with("ERROR"), "Step 1 error: {r1}");
    let lower1 = r1.to_lowercase();
    if lower1.contains("не найден") || lower1.contains("not found") || lower1.contains("0 key")
    {
        eprintln!("SKIP pm_21: no mistral keys in store");
        restore_config(&snapshot).await;
        return;
    }

    // Step 2: switch default to mistral
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r2 = send_and_wait(
        &mut tx,
        &mut rx,
        "переключи основного провайдера на mistral mistral-large-latest",
    )
    .await;
    assert!(!r2.starts_with("ERROR"), "Step 2 error: {r2}");
    let lower2 = r2.to_lowercase();
    assert!(
        lower2.contains("mistral")
            || lower2.contains("переключ")
            || lower2.contains("установлен")
            || lower2.contains("default")
            || lower2.contains("switch")
            || lower2.contains("set_default"),
        "Step 2 should confirm switch to mistral: {r2}"
    );

    restore_config(&snapshot).await;
}

/// Bot must actually connect minimax — find key, test it, add to fallback.
/// Uses multi-turn to guide the bot through the workflow if needed.
/// Verified by checking config AFTER the bot responds.
#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_22_minimax_connect_end_to_end() {
    let snapshot = snapshot_config().await;

    // If minimax is already configured, the test goal is already achieved.
    if snapshot.contains("minimax") {
        eprintln!("SKIP pm_22: minimax already in config (goal achieved)");
        return;
    }

    let (mut tx, mut rx) = connect().await;

    // Step 1: find key
    let r1 = send_and_wait(&mut tx, &mut rx, "найди рабочий ключ minimax").await;
    assert!(!r1.starts_with("ERROR"), "Step 1 error: {r1}");

    // Bot must NOT refuse
    let lower = r1.to_lowercase();
    let refusal_phrases = [
        "не могу подключить",
        "невозможно",
        "не поддерживается",
        "требует специальн",
        "техническ",
        "cannot connect",
        "not supported",
    ];
    for phrase in &refusal_phrases {
        assert!(
            !lower.contains(phrase),
            "Bot refused with '{phrase}' instead of attempting tools: {r1}"
        );
    }

    if lower.contains("не найден") || lower.contains("not found") || lower.contains("0 key")
    {
        eprintln!("SKIP pm_22: no minimax keys in store");
        restore_config(&snapshot).await;
        return;
    }

    // Step 2: add to fallback and test
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r2 = send_and_wait(
        &mut tx,
        &mut rx,
        "добавь найденный ключ minimax в фоллбэк и протестируй MiniMax-M1",
    )
    .await;
    assert!(!r2.starts_with("ERROR"), "Step 2 error: {r2}");

    // THE REAL CHECK: minimax must now be in the config fallback chain
    let config_after = snapshot_config().await;
    assert!(
        config_after.contains("minimax"),
        "Bot did NOT actually add minimax to config.\nStep 1: {r1}\nStep 2: {r2}"
    );

    restore_config(&snapshot).await;
}

/// Bot should discover available MiniMax models when asked about a non-existent one.
/// Must use provider_models or web_search, not blindly test the unknown model name.
#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_23_unknown_model_discovery() {
    let (mut tx, mut rx) = connect().await;
    let resp = send_and_wait(
        &mut tx,
        &mut rx,
        "Модель minimax-2.7HIGHSPEED существует? Проверь какие модели реально есть у MiniMax.",
    )
    .await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    // Bot should mention real MiniMax models or attempt to look them up
    assert!(
        lower.contains("minimax-m1")
            || lower.contains("minimax-text")
            || lower.contains("abab")
            || lower.contains("не существует")
            || lower.contains("не найден")
            || lower.contains("not found")
            || lower.contains("доступн")
            || lower.contains("provider_models"),
        "Bot should discover real models or report non-existence: {resp}"
    );
}

/// Telegram progress trimming: send a multi-tool task via Telegram,
/// verify that the progress message is trimmed to last 10 lines with "... +N" prefix.
///
/// Requires: telethon installed, TELEGRAM_API_ID/HASH env vars, valid session file.
#[tokio::test]
#[ignore = "requires running ZeroClaw daemon + Telegram session"]
async fn pm_24_telegram_progress_trimming() {
    // Cooldown before test (daemon may need time after previous tests)
    tokio::time::sleep(Duration::from_secs(5)).await;
    // Build path to the Python helper
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = manifest.join("tests/telegram_progress_e2e.py");
    assert!(script.exists(), "Missing {}", script.display());

    // Run the telethon script (up to 12 min — bot may be slow with rate limits)
    let output = tokio::process::Command::new("python3")
        .arg(&script)
        .env(
            "TELEGRAM_API_ID",
            std::env::var("TELEGRAM_API_ID").expect("TELEGRAM_API_ID not set"),
        )
        .env(
            "TELEGRAM_API_HASH",
            std::env::var("TELEGRAM_API_HASH").expect("TELEGRAM_API_HASH not set"),
        )
        .output()
        .await
        .expect("Failed to run telegram_progress_e2e.py");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    assert!(
        output.status.success(),
        "Script failed (exit={}):\nstdout: {stdout}\nstderr: {stderr}",
        output.status
    );

    // Parse JSON result
    let result: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("Failed to parse script output as JSON: {e}\nstdout: {stdout}\nstderr: {stderr}")
    });

    // Check for script-level errors
    if let Some(err) = result["error"].as_str() {
        panic!("Telegram script error: {err}");
    }

    let progress_edits = result["progress_edits"].as_u64().unwrap_or(0);
    let total_edits = result["total_edits"].as_u64().unwrap_or(0);
    eprintln!(
        "pm_24: progress_edits={progress_edits}, total_edits={total_edits}, \
         max_progress_lines={}, saw_plus_marker={}",
        result["max_progress_lines"], result["saw_plus_marker"]
    );

    // Must have seen progress edits (tool call notifications)
    assert!(
        progress_edits >= 3,
        "Expected at least 3 progress edits, got {progress_edits}. \
         Bot may not have used enough tools."
    );

    // The key assertion: progress was trimmed (saw "... +N действий" marker)
    assert!(
        result["progress_trimmed"].as_bool().unwrap_or(false),
        "Progress message was NOT trimmed (no '... +N' marker seen). \
         max_progress_lines={}, progress_edits={progress_edits}",
        result["max_progress_lines"]
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
        lower.contains("gemini")
            || lower.contains("default")
            || lower.contains("основн")
            || lower.contains("провайдер")
            || lower.contains("provider_status")
            || lower.contains("google"),
        "Response should mention current default: {resp}"
    );
}
