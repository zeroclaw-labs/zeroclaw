//! The application layer's runtime capabilities.
//!
//! The runtime obtains model providers, memory, tools, outbound channels and
//! the observer through [`RuntimeCapabilities`] rather than constructing them
//! itself (see `docs/book/src/architecture/runtime-composition.md`). This
//! module builds the set the `zeroclaw` binary hands to the runtime entry
//! points it calls.

use std::sync::Arc;

use zeroclaw_config::schema::Config;
use zeroclaw_runtime::composition::RuntimeCapabilities;

/// The capabilities the `zeroclaw` binary runs the runtime with.
///
/// Today these are the runtime's config-backed sources
/// ([`zeroclaw_runtime::composition::defaults`]) with the observer the config
/// selects: the same set the runtime's compatibility adapters build, so an
/// entry point that moves from an adapter onto this set constructs and
/// switches providers, opens memory and builds tools exactly as before.
pub struct DefaultCapabilities;

impl DefaultCapabilities {
    /// Build the capabilities for one config generation.
    ///
    /// A config apply that changes capability wiring builds a new set for the
    /// new generation; a turn keeps the set it started with.
    pub fn from_config(config: &Config) -> RuntimeCapabilities {
        let observer = zeroclaw_runtime::observability::create_observer(&config.observability);
        RuntimeCapabilities::config_backed_with_observer(Arc::from(observer))
    }
}

/// Run the application's agent command with its default capability factory.
///
/// This is the shared caller for the CLI and application-level turn tests.
/// The CLI has no resolved principal. Runtime admission, policy resolution,
/// provider overrides, peripherals and session handling remain runtime-owned.
#[allow(clippy::too_many_arguments)]
pub fn run_agent(
    config: Config,
    agent_alias: &str,
    message: Option<String>,
    provider_override: Option<String>,
    model_override: Option<String>,
    temperature: Option<f64>,
    peripheral_overrides: Vec<String>,
    interactive: bool,
    session_state_file: Option<std::path::PathBuf>,
    allowed_tools: Option<Vec<String>>,
    origin: zeroclaw_api::ingress::TurnOrigin,
    overrides: zeroclaw_runtime::agent::loop_::AgentRunOverrides,
) -> impl std::future::Future<Output = anyhow::Result<String>> + '_ {
    let capabilities = DefaultCapabilities::from_config(&config);
    zeroclaw_runtime::agent::run_with_capabilities(
        config,
        capabilities,
        None,
        agent_alias,
        message,
        provider_override,
        model_override,
        temperature,
        peripheral_overrides,
        interactive,
        session_state_file,
        allowed_tools,
        origin,
        overrides,
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use serde_json::{Value, json};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    use zeroclaw_config::multi_agent::{
        AgentMemoryConfig, AgentWorkspaceConfig, MemoryBackendKind,
    };
    use zeroclaw_config::schema::{
        AliasedAgentConfig, Config, CustomModelProviderConfig, ModelProviderConfig,
        RiskProfileConfig, RuntimeProfileConfig,
    };

    const AGENT: &str = "parity";
    const REPLY: &str = "Parity reply from the fixture.";

    struct FixturePeripheral;

    zeroclaw_api::mock_tool_attribution!(FixturePeripheral);

    #[async_trait::async_trait]
    impl zeroclaw_api::tool::Tool for FixturePeripheral {
        fn name(&self) -> &str {
            "fixture_peripheral"
        }

        fn description(&self) -> &str {
            "Peripheral fixture excluded by the agent policy"
        }

        fn parameters_schema(&self) -> Value {
            json!({"type": "object"})
        }

        async fn execute(&self, _args: Value) -> anyhow::Result<zeroclaw_api::tool::ToolResult> {
            panic!("an excluded peripheral must never execute")
        }
    }

    /// An OpenAI-compatible chat endpoint that answers every request with
    /// [`REPLY`], streamed when the request asks for a stream.
    struct FixtureReply;

    impl Respond for FixtureReply {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
            let usage = json!({"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15});
            if body.get("stream").and_then(Value::as_bool) == Some(true) {
                let chunk = json!({"id": "chatcmpl-parity", "model": "fixture-model",
                    "choices": [{"index": 0, "delta": {"role": "assistant", "content": REPLY}}]});
                let last = json!({"id": "chatcmpl-parity", "model": "fixture-model",
                    "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                    "usage": usage});
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(format!("data: {chunk}\n\ndata: {last}\n\ndata: [DONE]\n\n"))
            } else {
                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "chatcmpl-parity",
                    "object": "chat.completion",
                    "model": "fixture-model",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": REPLY},
                        "finish_reason": "stop"
                    }],
                    "usage": usage,
                }))
            }
        }
    }

    fn fixture_config(root: &Path, provider_url: &str) -> Config {
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("fixture workspace");
        let mut config = Config {
            data_dir: workspace.clone(),
            config_path: root.join("config.toml"),
            ..Config::default()
        };
        config.memory.backend = "none".to_string();
        config.memory.auto_save = false;
        config.memory.response_cache_enabled = false;
        config.reliability.provider_retries = 0;
        config.reliability.provider_backoff_ms = 0;
        config.providers.models.custom.insert(
            "fixture".to_string(),
            CustomModelProviderConfig {
                base: ModelProviderConfig {
                    api_key: Some("parity-test-key".to_string()),
                    uri: Some(provider_url.to_string()),
                    model: Some("fixture-model".to_string()),
                    temperature: Some(0.0),
                    ..ModelProviderConfig::default()
                },
            },
        );
        config.risk_profiles.insert(
            "fixture".to_string(),
            RiskProfileConfig {
                allowed_tools: vec!["__parity_fixture_no_tools__".to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.runtime_profiles.insert(
            "fixture".to_string(),
            RuntimeProfileConfig {
                max_tool_iterations: 1,
                ..RuntimeProfileConfig::default()
            },
        );
        config.agents.insert(
            AGENT.to_string(),
            AliasedAgentConfig {
                model_provider: "custom.fixture".into(),
                risk_profile: "fixture".into(),
                runtime_profile: "fixture".into(),
                memory: AgentMemoryConfig {
                    backend: MemoryBackendKind::None,
                },
                workspace: AgentWorkspaceConfig {
                    path: Some(workspace),
                    ..AgentWorkspaceConfig::default()
                },
                ..AliasedAgentConfig::default()
            },
        );
        config
    }

    /// The parts of a provider request that must not depend on which entry
    /// point built the turn. The system prompt carries the wall clock, so the
    /// message text is compared by role only.
    fn request_shape(request: &Request) -> Value {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let roles: Vec<Value> = body["messages"]
            .as_array()
            .map(|messages| messages.iter().map(|m| m["role"].clone()).collect())
            .unwrap_or_default();
        let tools: Vec<Value> = body["tools"]
            .as_array()
            .map(|tools| {
                tools
                    .iter()
                    .map(|t| t["function"]["name"].clone())
                    .collect()
            })
            .unwrap_or_default();
        json!({
            "path": request.url.path(),
            "authorization": request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            "model": body["model"],
            "stream": body["stream"],
            "roles": roles,
            "tools": tools,
        })
    }

    /// Runs one non-interactive turn and returns the reply and the shapes of
    /// the provider requests it made.
    async fn run_turn(use_default_capabilities: bool) -> (String, Vec<Value>) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(FixtureReply)
            .mount(&server)
            .await;
        let root = tempfile::TempDir::new().expect("fixture root");
        let config = fixture_config(root.path(), &format!("{}/v1", server.uri()));
        let origin = zeroclaw_api::ingress::TurnOrigin::Interactive;
        let overrides = zeroclaw_runtime::agent::loop_::AgentRunOverrides::default();
        let message = Some("Say hello to the parity test.".to_string());

        let reply = if use_default_capabilities {
            Box::pin(super::run_agent(
                config,
                AGENT,
                message,
                None,
                None,
                None,
                Vec::new(),
                false,
                None,
                None,
                origin,
                overrides,
            ))
            .await
        } else {
            Box::pin(zeroclaw_runtime::agent::run(
                config,
                AGENT,
                message,
                None,
                None,
                None,
                Vec::new(),
                false,
                None,
                None,
                origin,
                overrides,
            ))
            .await
        }
        .expect("the fixture turn completes");

        let requests = server
            .received_requests()
            .await
            .expect("request recording is on");
        (reply, requests.iter().map(request_shape).collect())
    }

    /// The application factory preserves alias-specific endpoints, models,
    /// credentials and fallback selection through the same caller as the CLI.
    #[test]
    fn application_agent_preserves_provider_override_and_fallback() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let connections = Arc::new(AtomicUsize::new(0));
        let factory_connections = Arc::clone(&connections);
        zeroclaw_runtime::agent::loop_::register_peripheral_tools_fn(Box::new(move |config| {
            let connections = Arc::clone(&factory_connections);
            Box::pin(async move {
                if !config.enabled
                    || !config
                        .boards
                        .iter()
                        .any(|board| board.board == "composition-fixture")
                {
                    return Ok(Vec::new());
                }
                connections.fetch_add(1, Ordering::SeqCst);
                Ok(vec![
                    Box::new(FixturePeripheral) as Box<dyn zeroclaw_api::tool::Tool>
                ])
            })
        }));
        std::thread::Builder::new()
            .name("application-composition-routing".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_stack_size(8 * 1024 * 1024)
                    .enable_all()
                    .build()
                    .expect("application runtime");
                runtime.block_on(async {
                    for override_provider in [true, false] {
                        let primary = MockServer::start().await;
                        Mock::given(method("POST"))
                            .and(path("/v1/chat/completions"))
                            .respond_with(ResponseTemplate::new(503))
                            .mount(&primary)
                            .await;
                        let secondary = MockServer::start().await;
                        Mock::given(method("POST"))
                            .and(path("/v1/chat/completions"))
                            .respond_with(FixtureReply)
                            .mount(&secondary)
                            .await;
                        let root = tempfile::TempDir::new().expect("fixture root");
                        let mut config =
                            fixture_config(root.path(), &format!("{}/v1", primary.uri()));
                        config.peripherals.enabled = true;
                        config.peripherals.boards.push(
                            zeroclaw_config::schema::PeripheralBoardConfig {
                                board: "composition-fixture".to_string(),
                                ..Default::default()
                            },
                        );
                        config.providers.models.custom.insert(
                            "secondary".to_string(),
                            CustomModelProviderConfig {
                                base: ModelProviderConfig {
                                    api_key: Some("secondary-test-key".to_string()),
                                    uri: Some(format!("{}/v1", secondary.uri())),
                                    model: Some("secondary-model".to_string()),
                                    temperature: Some(0.0),
                                    ..ModelProviderConfig::default()
                                },
                            },
                        );
                        if !override_provider {
                            config
                                .providers
                                .models
                                .custom
                                .get_mut("fixture")
                                .expect("primary alias")
                                .base
                                .fallback = vec!["custom.secondary".into()];
                        }
                        let reply = tokio::time::timeout(
                            Duration::from_secs(60),
                            Box::pin(super::run_agent(
                                config,
                                AGENT,
                                Some("Use the selected provider.".to_string()),
                                override_provider.then(|| "custom.secondary".to_string()),
                                None,
                                None,
                                Vec::new(),
                                true,
                                None,
                                None,
                                zeroclaw_api::ingress::TurnOrigin::Interactive,
                                zeroclaw_runtime::agent::loop_::AgentRunOverrides::default(),
                            )),
                        )
                        .await
                        .expect("application turn timed out")
                        .expect("application turn completes");
                        assert!(reply.contains(REPLY));
                        let requests = secondary.received_requests().await.expect("recording");
                        assert!(!requests.is_empty(), "selected alias must receive the turn");
                        for request in &requests {
                            let shape = request_shape(request);
                            assert_eq!(shape["authorization"], "Bearer secondary-test-key");
                            assert_eq!(
                                shape["model"],
                                if override_provider {
                                    "fixture-model"
                                } else {
                                    "secondary-model"
                                }
                            );
                            assert_eq!(shape["tools"], json!([]));
                        }
                        let attempted_primary =
                            primary.received_requests().await.expect("recording");
                        assert_eq!(attempted_primary.is_empty(), override_provider);
                    }
                    assert_eq!(connections.load(Ordering::SeqCst), 2);
                });
            })
            .expect("spawn application thread")
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    }

    /// Moving the CLI onto [`DefaultCapabilities`] must not change a turn: the
    /// adapter path and the default-capability path reach the provider with
    /// the same requests and return the same reply.
    #[test]
    fn default_capabilities_turn_matches_the_adapter_turn() {
        // Building a runtime agent overflows the default test-thread stack on
        // Linux, so the turns run on a runtime with large worker stacks.
        std::thread::Builder::new()
            .name("default-capabilities-parity".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_stack_size(8 * 1024 * 1024)
                    .enable_all()
                    .build()
                    .expect("parity runtime");
                runtime.block_on(async {
                    let adapter =
                        tokio::time::timeout(Duration::from_secs(60), Box::pin(run_turn(false)))
                            .await
                            .expect("adapter turn timed out");
                    let default =
                        tokio::time::timeout(Duration::from_secs(60), Box::pin(run_turn(true)))
                            .await
                            .expect("default-capability turn timed out");

                    assert!(
                        adapter.0.contains(REPLY),
                        "the adapter turn returns the fixture reply: {:?}",
                        adapter.0
                    );
                    assert_eq!(default.0, adapter.0, "replies differ between entry points");
                    assert!(
                        !adapter.1.is_empty(),
                        "the adapter turn reached the provider"
                    );
                    assert_eq!(
                        default.1, adapter.1,
                        "provider requests differ between entry points"
                    );
                });
            })
            .expect("spawn parity thread")
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    }
}
