//! Integration test for #5415 (follow-up to #5456).
//!
//! Reproduces the chat → scheduled-task leak at the agent-loop level. The
//! daemon heartbeat path (`crates/zeroclaw-runtime/src/daemon/mod.rs`) calls
//! `agent::run(..., interactive=false, session_state_file=None, ...)`. With
//! no session_state_file, the agent's `memory_session_id` is `None`, so the
//! SQLite recall inside `build_context` is unscoped and returns Conversation
//! memories from any channel session. Before the fix, those entries were
//! embedded into the prompt sent to the provider.
//!
//! (The cron path generates a fresh `cron-{uuid}` session per run, so SQLite
//! session scoping happens to mask the leak there under the SQLite backend.
//! The build_context filter is still required for defense in depth — the
//! markdown backend ignores session_id entirely (memory/markdown.rs:163), and
//! the heartbeat path has no session scoping at all.)
//!
//! Setup
//! 1. Spin up a minimal axum-based OpenAI-compatible server that records every
//!    `/chat/completions` request body.
//! 2. Plant a Conversation entry in `SqliteMemory` under a non-autosave key.
//! 3. Build a `Config` whose fallback provider is `custom:<mock-url>`.
//! 4. Call `agent::run` with daemon-heartbeat parameters (`interactive=false`,
//!    `session_state_file=None`).
//! 5. Assert no captured request body contains the planted unique sentinel.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::{Router, extract::State, routing::post};
use tempfile::TempDir;
use tokio::sync::Mutex as AsyncMutex;
use zeroclaw_config::providers::ProvidersConfig;
use zeroclaw_config::schema::{Config, ModelProviderConfig};
use zeroclaw_memory::{Memory, MemoryCategory, SqliteMemory};

// Unique sentinel that exists ONLY in the planted Conversation entry — must
// not appear in the cron prompt or any system prompt. If it surfaces in the
// captured request body, the only path it could have taken is build_context's
// recall + injection.
const SECRET_SENTINEL: &str = "blue-walrus-7421-conversation-leak-canary";
const FAKE_OPENAI_RESPONSE: &str = r#"{"id":"chatcmpl-test","object":"chat.completion","created":0,"model":"test-model","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;

type CapturedBodies = Arc<AsyncMutex<Vec<String>>>;

async fn handle_chat(State(captured): State<CapturedBodies>, body: String) -> &'static str {
    captured.lock().await.push(body);
    FAKE_OPENAI_RESPONSE
}

async fn spawn_mock_provider() -> (SocketAddr, CapturedBodies) {
    let captured: CapturedBodies = Arc::new(AsyncMutex::new(Vec::new()));
    let app = Router::new()
        .route("/chat/completions", post(handle_chat))
        .with_state(captured.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });
    (addr, captured)
}

#[tokio::test]
async fn scheduled_run_does_not_leak_conversation_memory_into_provider_request() {
    let tmp = TempDir::new().unwrap();
    let workspace_dir = tmp.path().join("workspace");
    tokio::fs::create_dir_all(&workspace_dir).await.unwrap();

    // ── Mock provider ───────────────────────────────────────────────
    let (addr, captured) = spawn_mock_provider().await;
    let provider_name = format!("custom:http://{addr}");

    // ── Plant a chat-origin Conversation memory ────────────────────
    // Keys like "discord:guild:chan:msg-N" come from real channel handlers
    // (gateway/lib.rs auto-save); they bypass the existing autosave-key
    // skip-list because they don't match user_msg_*/assistant_resp_*.
    //
    // session_id is `None` to model the user-reported repro: Conversation
    // entries that lack session scoping (older data, channels that don't
    // populate session_id, or backends without strict session filtering).
    // With a session_id set, SQLite's recall filter scopes it out before
    // build_context runs — the bug manifests precisely when scoping fails.
    {
        let mem = SqliteMemory::new(&workspace_dir).unwrap();
        mem.store(
            "discord:guild:chan:msg-42",
            // Includes overlap words ("reminder", "today") so the keyword
            // search returns this entry for the cron prompt below, plus the
            // unique SECRET_SENTINEL the assertion looks for.
            &format!(
                "Reminder from today's chat: {SECRET_SENTINEL} — do not surface this in scheduled tasks."
            ),
            MemoryCategory::Conversation,
            None,
        )
        .await
        .unwrap();
    }

    // ── Config pointing the agent at the mock provider ─────────────
    let mut models = HashMap::new();
    models.insert(
        provider_name.clone(),
        ModelProviderConfig {
            api_key: Some("test-key".to_string()),
            model: Some("test-model".to_string()),
            ..Default::default()
        },
    );
    let mut config = Config {
        workspace_dir: workspace_dir.clone(),
        config_path: tmp.path().join("config.toml"),
        providers: ProvidersConfig {
            fallback: Some(provider_name.clone()),
            models,
            ..Default::default()
        },
        ..Config::default()
    };
    // No retries / no waits — fail fast if the mock has issues, and don't
    // multiply the captured bodies during this test.
    config.reliability.scheduler_retries = 0;
    config.reliability.provider_retries = 0;
    // Drop the relevance threshold so the recall surfaces the planted entry
    // deterministically; production threshold is 0.4 and would filter out
    // weakly-matching entries before build_context's category filter runs.
    config.memory.min_relevance_score = 0.0;

    // ── Drive the daemon-heartbeat invocation pattern ──────────────
    // Matches `crates/zeroclaw-runtime/src/daemon/mod.rs:476` / `:599`:
    //   crate::agent::run(
    //       config, Some(prompt), None, None, temp,
    //       vec![], false, None, None,
    //   )
    // `interactive=false` + `session_state_file=None` is exactly the heartbeat
    // shape that bypasses session scoping inside `build_context`.
    let prompt = "Any reminders to surface today? Pull anything relevant from memory.".to_string();
    let run_result = zeroclaw_runtime::agent::run(
        config,
        Some(prompt),
        None,
        None,
        0.7,
        vec![],
        false,
        None,
        None,
    )
    .await;
    let (success, output) = match run_result {
        Ok(out) => (true, out),
        Err(err) => (false, format!("agent run errored: {err:#}")),
    };

    // We don't strictly require success — even if the agent loop bails after
    // the first chat round, the captured request body is what we audit.
    let bodies = captured.lock().await;
    assert!(
        !bodies.is_empty(),
        "mock provider received zero requests — agent run never reached the LLM. \
         job success={success}, output={output}"
    );

    for (i, body) in bodies.iter().enumerate() {
        assert!(
            !body.contains(SECRET_SENTINEL),
            "Conversation memory leaked into scheduled-run LLM request #{i}: \
             sentinel {SECRET_SENTINEL:?} found in body. Full body:\n{body}"
        );
    }
}
