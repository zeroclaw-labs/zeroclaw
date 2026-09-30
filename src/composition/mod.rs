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

#[cfg(test)]
mod tests {
    use super::DefaultCapabilities;

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
            let capabilities = DefaultCapabilities::from_config(&config);
            Box::pin(zeroclaw_runtime::agent::run_with_capabilities(
                config,
                capabilities,
                None,
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
