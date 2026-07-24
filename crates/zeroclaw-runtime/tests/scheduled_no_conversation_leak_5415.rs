use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::{Router, extract::State, routing::post};
use tempfile::TempDir;
use tokio::sync::Mutex as AsyncMutex;
use zeroclaw_config::schema::{AliasedAgentConfig, Config, RiskProfileConfig};
use zeroclaw_memory::{Memory, MemoryCategory, SqliteMemory};

// Unique sentinel that exists ONLY in the planted Conversation entry: it must
// not appear in the cron prompt or any system prompt. If it surfaces in the
// captured request body, the only path it could have taken is the engine's
// memory-context recall + injection.
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
    zeroclaw_spawn::spawn!(async move {
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
    let provider_uri = format!("http://{addr}");
    // Canonical typed-family slot. The agent's `model_provider` references
    // the alias by `<type>.<alias>` (here `custom.default`).
    let provider_type = "custom";

    {
        let mem = SqliteMemory::new("sqlite", &workspace_dir).unwrap();
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

    let mut providers = zeroclaw_config::providers::Providers::default();
    {
        let base = providers
            .models
            .ensure(provider_type, "default")
            .expect("`custom` slot must exist on ModelProviders");
        base.api_key = Some("test-key".to_string());
        base.model = Some("test-model".to_string());
        base.uri = Some(provider_uri.clone());
    }
    let mut agents = HashMap::new();
    agents.insert(
        "default".to_string(),
        AliasedAgentConfig {
            enabled: true,
            model_provider: format!("{provider_type}.default").into(),
            risk_profile: "default".into(),
            ..Default::default()
        },
    );
    // PR branch requires every agent to point at a configured risk_profile;
    // wire up a permissive entry so the agent loop reaches the LLM call we
    // care about auditing here.
    let mut risk_profiles = HashMap::new();
    risk_profiles.insert("default".to_string(), RiskProfileConfig::default());
    let mut config = Config {
        data_dir: workspace_dir.clone(),
        config_path: tmp.path().join("config.toml"),
        providers,
        agents,
        risk_profiles,
        ..Config::default()
    };
    // No retries / no waits — fail fast if the mock has issues, and don't
    // multiply the captured bodies during this test.
    config.reliability.scheduler_retries = 0;
    config.reliability.provider_retries = 0;
    // Drop the relevance threshold so the recall surfaces the planted entry
    // deterministically; production threshold is 0.4 and would filter out
    // weakly-matching entries before the engine renderer's category filter
    // runs.
    config.memory.min_relevance_score = 0.0;

    let prompt = "Any reminders to surface today? Pull anything relevant from memory.".to_string();
    let run_result = zeroclaw_runtime::agent::run(
        config,
        "default",
        Some(prompt),
        None,
        None,
        Some(0.7),
        vec![],
        false,
        None,
        None,
        zeroclaw_api::ingress::TurnOrigin::Daemon,
        zeroclaw_runtime::agent::loop_::AgentRunOverrides::default(),
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
