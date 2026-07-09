//! Integration test a cron job with `uses_memory = false` must be
//! memory-free end to end, not merely opted out of the context preamble.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::{Router, extract::State, routing::post};
use tempfile::TempDir;
use tokio::sync::Mutex as AsyncMutex;
use zeroclaw_config::schema::{AliasedAgentConfig, Config, RiskProfileConfig};
use zeroclaw_memory::{Memory, MemoryCategory, SqliteMemory};
use zeroclaw_runtime::agent::loop_::AgentRunOverrides;

// Present ONLY in the planted memory entry: if it surfaces in a provider
// request body, recall injected it. A memory-free run must never surface it.
const SECRET_SENTINEL: &str = "amber-lynx-8695-uses-memory-free-canary";
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

async fn config_with_planted_memory(provider_uri: &str, workspace_dir: &std::path::Path) -> Config {
    let provider_type = "custom";
    {
        let mem = SqliteMemory::new("sqlite", workspace_dir).unwrap();
        mem.store(
            "core:reminder:8695",
            &format!(
                "Reminder from memory: {SECRET_SENTINEL} — surface today's scheduled reminder."
            ),
            MemoryCategory::Core,
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
        base.uri = Some(provider_uri.to_string());
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
    let mut risk_profiles = HashMap::new();
    risk_profiles.insert("default".to_string(), RiskProfileConfig::default());
    let mut config = Config {
        data_dir: workspace_dir.to_path_buf(),
        config_path: workspace_dir.join("config.toml"),
        providers,
        agents,
        risk_profiles,
        ..Config::default()
    };
    config.reliability.scheduler_retries = 0;
    config.reliability.provider_retries = 0;
    // Drop the relevance floor so the planted entry WOULD surface under a live
    // backend; the memory-free run must still keep it out.
    config.memory.min_relevance_score = 0.0;
    config
}

/// Drive one cron-origin `agent::run` and return the captured request bodies.
async fn run_cron_once(config: Config, overrides: AgentRunOverrides) -> Vec<String> {
    let (addr, captured) = spawn_mock_provider().await;
    let provider_uri = format!("http://{addr}");
    // Re-point the provider at this run's mock instance.
    let mut config = config;
    {
        let base = config
            .providers
            .models
            .ensure("custom", "default")
            .expect("`custom` slot must exist");
        base.uri = Some(provider_uri);
    }

    let prompt = "Any reminders to surface today? Pull anything relevant from memory.".to_string();
    let _ = zeroclaw_runtime::agent::run(
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
        zeroclaw_api::ingress::TurnOrigin::Cron,
        overrides,
    )
    .await;

    let bodies = captured.lock().await;
    bodies.clone()
}

fn body_advertises_memory_tool(body: &str) -> bool {
    zeroclaw_tools::MEMORY_TOOL_NAMES
        .iter()
        .any(|name| body.contains(name))
}

#[tokio::test]
async fn uses_memory_false_cron_run_has_no_memory_backend_or_tools() {
    let tmp = TempDir::new().unwrap();
    let workspace_dir = tmp.path().join("workspace");
    tokio::fs::create_dir_all(&workspace_dir).await.unwrap();

    // The scheduler builds this exact override shape for `uses_memory = false`.
    let config = config_with_planted_memory("http://placeholder", &workspace_dir).await;
    let overrides = AgentRunOverrides {
        memory_free: true,
        suppress_memory_inject: true,
        ..Default::default()
    };
    let bodies = run_cron_once(config, overrides).await;

    assert!(
        !bodies.is_empty(),
        "mock provider received zero requests — the memory-free cron run never reached the LLM"
    );
    for (i, body) in bodies.iter().enumerate() {
        assert!(
            !body_advertises_memory_tool(body),
            "memory-free cron run advertised a memory_* tool in provider request #{i}; \
             `uses_memory = false` must drop the persistent memory tools. Body:\n{body}"
        );
        assert!(
            !body.contains(SECRET_SENTINEL),
            "memory-free cron run leaked a planted memory entry into provider request #{i}; \
             recall must be inert on the NoneMemory backend. Body:\n{body}"
        );
    }
}

#[tokio::test]
async fn uses_memory_true_cron_run_keeps_memory_tools() {
    // Control: the identical setup with a live backend (`uses_memory = true`)
    // MUST advertise the memory tools. This proves the exclusion above is
    // meaningful and that `memory_free` — not some unrelated gating — is what
    // removes them.
    let tmp = TempDir::new().unwrap();
    let workspace_dir = tmp.path().join("workspace");
    tokio::fs::create_dir_all(&workspace_dir).await.unwrap();

    let config = config_with_planted_memory("http://placeholder", &workspace_dir).await;
    let overrides = AgentRunOverrides {
        memory_free: false,
        suppress_memory_inject: false,
        ..Default::default()
    };
    let bodies = run_cron_once(config, overrides).await;

    assert!(
        !bodies.is_empty(),
        "mock provider received zero requests — the live cron run never reached the LLM"
    );
    assert!(
        bodies.iter().any(|body| body_advertises_memory_tool(body)),
        "live cron run (`uses_memory = true`) advertised no memory_* tool — the control is \
         invalid, so the memory-free assertion above proves nothing. Bodies:\n{bodies:#?}"
    );
}
