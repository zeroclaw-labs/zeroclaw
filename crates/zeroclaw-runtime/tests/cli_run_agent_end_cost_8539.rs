//! Integration test the CLI `agent::run` entry point must report the priced
//! `cost_usd` on its `AgentEnd`, not just `tokens_used`.
//!
//! `run` builds its observer internally through `create_observer` and takes no
//! observer parameter, so its `AgentEnd` is the one emission site with no
//! in-process injection point. The seam used here instead: `create_observer`
//! wraps every configured backend in a `TeeObserver`, which fans each event
//! out to the process-wide broadcast hook. Installing a capturing observer via
//! `set_scoped_broadcast_hook` therefore intercepts the event without touching
//! production code. (The gateway SSE tests intercept the same hook through its
//! unscoped `set_broadcast_hook` sibling under a shared lock; the scoped guard
//! is preferable here because this file is its own test binary.)
//!
//! `interactive` is false, so the emitted `channel` label is `daemon` rather
//! than `cli`; both interactive and one-shot runs share the single `AgentEnd`
//! literal under test, so coverage is unaffected.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::{Router, extract::State, routing::post};
use parking_lot::Mutex;
use tempfile::TempDir;
use zeroclaw_config::schema::{AliasedAgentConfig, Config, RiskProfileConfig};
use zeroclaw_runtime::observability::traits::{Observer, ObserverEvent, ObserverMetric};

const INPUT_TOKENS: u64 = 1000;
const OUTPUT_TOKENS: u64 = 200;
const INPUT_PER_MTOK: f64 = 3.0;
const OUTPUT_PER_MTOK: f64 = 15.0;

const FAKE_OPENAI_RESPONSE: &str = r#"{"id":"chatcmpl-test","object":"chat.completion","created":0,"model":"test-model","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1000,"completion_tokens":200,"total_tokens":1200}}"#;

#[derive(Default)]
struct CapturingObserver {
    events: Mutex<Vec<ObserverEvent>>,
}

impl Observer for CapturingObserver {
    fn record_event(&self, event: &ObserverEvent) {
        self.events.lock().push(event.clone());
    }
    fn record_metric(&self, _metric: &ObserverMetric) {}
    fn name(&self) -> &str {
        "capturing"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

async fn handle_chat(State(()): State<()>, _body: String) -> &'static str {
    FAKE_OPENAI_RESPONSE
}

async fn spawn_mock_provider() -> SocketAddr {
    let app = Router::new()
        .route("/chat/completions", post(handle_chat))
        .with_state(());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    zeroclaw_spawn::spawn!(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });
    addr
}

#[tokio::test]
async fn cli_run_agent_end_carries_priced_cost_usd() {
    let tmp = TempDir::new().unwrap();
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace_dir).unwrap();

    let addr = spawn_mock_provider().await;

    let mut providers = zeroclaw_config::providers::Providers::default();
    {
        let base = providers
            .models
            .ensure("custom", "default")
            .expect("`custom` slot must exist on ModelProviders");
        base.api_key = Some("test-key".to_string());
        base.model = Some("test-model".to_string());
        base.uri = Some(format!("http://{addr}"));
        // Keyed to match `build_model_provider_pricing`, which indexes on
        // `<provider_type>.<alias>` then `<model>.<input|output>`.
        base.pricing
            .insert("test-model.input".to_string(), INPUT_PER_MTOK);
        base.pricing
            .insert("test-model.output".to_string(), OUTPUT_PER_MTOK);
    }
    let mut agents = HashMap::new();
    agents.insert(
        "default".to_string(),
        AliasedAgentConfig {
            enabled: true,
            model_provider: "custom.default".into(),
            risk_profile: "default".into(),
            ..Default::default()
        },
    );
    let mut risk_profiles = HashMap::new();
    risk_profiles.insert("default".to_string(), RiskProfileConfig::default());
    let mut config = Config {
        data_dir: workspace_dir.clone(),
        config_path: workspace_dir.join("config.toml"),
        providers,
        agents,
        risk_profiles,
        ..Config::default()
    };
    config.reliability.scheduler_retries = 0;
    config.reliability.provider_retries = 0;
    // Without this the tool loop never builds a cost context and the turn is
    // untracked, which is the `None` case rather than the priced one.
    config.cost.enabled = true;
    config.memory.backend = "none".to_string();

    let capture = Arc::new(CapturingObserver::default());
    let _hook = zeroclaw_runtime::observability::set_scoped_broadcast_hook(
        Arc::clone(&capture) as Arc<dyn Observer>
    );

    zeroclaw_runtime::agent::run(
        config,
        "default",
        Some("hello".to_string()),
        None,
        None,
        Some(0.0),
        vec![],
        false,
        None,
        None,
        zeroclaw_api::ingress::TurnOrigin::Cron,
        zeroclaw_runtime::agent::loop_::AgentRunOverrides::default(),
    )
    .await
    .expect("the mock provider turn must succeed");

    let events = capture.events.lock();
    let (tokens, cost) = events
        .iter()
        .find_map(|event| match event {
            ObserverEvent::AgentEnd {
                tokens_used,
                cost_usd,
                ..
            } => Some((tokens_used.clone(), *cost_usd)),
            _ => None,
        })
        .expect("the CLI run must emit AgentEnd");

    // Asserted alongside the cost so a harness that silently recorded no usage
    // fails here instead of passing a vacuous cost assertion.
    let tokens = tokens.expect("a tracked turn must report tokens_used");
    assert_eq!(tokens.input_tokens, INPUT_TOKENS);
    assert_eq!(tokens.output_tokens, OUTPUT_TOKENS);

    let expected = (INPUT_TOKENS as f64 * INPUT_PER_MTOK + OUTPUT_TOKENS as f64 * OUTPUT_PER_MTOK)
        / 1_000_000.0;
    let cost = cost.expect("a tracked turn must report Some(cost_usd), not None");
    assert!(
        (cost - expected).abs() < 1e-12,
        "AgentEnd cost_usd must equal the priced turn cost: got {cost}, want {expected}"
    );
}
