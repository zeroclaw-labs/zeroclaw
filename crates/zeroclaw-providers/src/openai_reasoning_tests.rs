#[cfg(test)]
mod native_effort {
    use crate::openai_reasoning::*;
    use std::collections::HashSet;

    #[test]
    fn all_neutral_levels_map_without_requiring_a_token_budget() {
        let supported = [
            NativeEffort::None,
            NativeEffort::Minimal,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ];
        for (level, expected) in [
            (ThinkingLevel::Off, NativeEffort::None),
            (ThinkingLevel::Minimal, NativeEffort::Minimal),
            (ThinkingLevel::Low, NativeEffort::Low),
            (ThinkingLevel::Medium, NativeEffort::Medium),
            (ThinkingLevel::High, NativeEffort::High),
            (ThinkingLevel::Max, NativeEffort::Max),
        ] {
            assert_eq!(
                map_level(level, &supported),
                Some(MappedEffort {
                    effort: expected,
                    substituted: false
                })
            );
        }
    }

    #[test]
    fn unsupported_levels_raise_to_the_next_supported_effort_or_cap_at_maximum() {
        for (level, supported, expected) in [
            (
                ThinkingLevel::Off,
                &[NativeEffort::Low, NativeEffort::High][..],
                NativeEffort::Low,
            ),
            (
                ThinkingLevel::Minimal,
                &[NativeEffort::None, NativeEffort::Low][..],
                NativeEffort::Low,
            ),
            (
                ThinkingLevel::Low,
                &[NativeEffort::Medium, NativeEffort::High][..],
                NativeEffort::Medium,
            ),
            (
                ThinkingLevel::High,
                &[NativeEffort::Low][..],
                NativeEffort::Low,
            ),
            (
                ThinkingLevel::Max,
                &[NativeEffort::Minimal, NativeEffort::High][..],
                NativeEffort::High,
            ),
        ] {
            assert_eq!(
                map_level(level, supported),
                Some(MappedEffort {
                    effort: expected,
                    substituted: true
                })
            );
        }
        assert_eq!(map_level(ThinkingLevel::Low, &[]), None);
    }

    #[test]
    fn public_and_subscription_efforts_are_resolved_independently() {
        assert_eq!(
            resolve(
                ReasoningSurface::PublicApi,
                ReasoningApi::Responses,
                "gpt-6.1-terra",
                ThinkingLevel::Off,
                false
            )
            .unwrap()
            .effort,
            NativeEffort::None
        );
        assert_eq!(
            resolve(
                ReasoningSurface::Subscription,
                ReasoningApi::Responses,
                "gpt-6.1-terra",
                ThinkingLevel::Off,
                false
            )
            .unwrap()
            .effort,
            NativeEffort::Low
        );
        assert_eq!(
            resolve(
                ReasoningSurface::Subscription,
                ReasoningApi::Responses,
                "gpt-6.1-sol",
                ThinkingLevel::Max,
                true
            )
            .unwrap()
            .effort,
            NativeEffort::Max
        );
        assert_eq!(
            resolve(
                ReasoningSurface::Subscription,
                ReasoningApi::Responses,
                "gpt-5.5",
                ThinkingLevel::Max,
                true
            )
            .unwrap()
            .effort,
            NativeEffort::XHigh
        );
        assert_eq!(
            resolve(
                ReasoningSurface::Subscription,
                ReasoningApi::Responses,
                "gpt-5-codex",
                ThinkingLevel::Max,
                false
            ),
            Err(UnmappedReason::UnknownModel)
        );
    }

    #[test]
    fn chat_tools_use_the_verified_effort_subset() {
        assert_eq!(
            resolve(
                ReasoningSurface::PublicApi,
                ReasoningApi::ChatCompletions,
                "gpt-6-sol",
                ThinkingLevel::Off,
                true
            )
            .unwrap()
            .effort,
            NativeEffort::None
        );
        assert_eq!(
            resolve(
                ReasoningSurface::PublicApi,
                ReasoningApi::ChatCompletions,
                "gpt-6-sol",
                ThinkingLevel::High,
                true
            )
            .unwrap()
            .effort,
            NativeEffort::None
        );
        assert_eq!(
            resolve(
                ReasoningSurface::PublicApi,
                ReasoningApi::ChatCompletions,
                "gpt-6.1-sol",
                ThinkingLevel::High,
                true
            ),
            Err(UnmappedReason::UnverifiedChatTools)
        );
        assert_eq!(
            resolve(
                ReasoningSurface::PublicApi,
                ReasoningApi::ChatCompletions,
                "gpt-5.4",
                ThinkingLevel::High,
                true
            )
            .unwrap()
            .effort,
            NativeEffort::High
        );
        assert_eq!(
            resolve(
                ReasoningSurface::PublicApi,
                ReasoningApi::ChatCompletions,
                "gpt-5.4-pro",
                ThinkingLevel::High,
                false
            ),
            Err(UnmappedReason::UnsupportedApi)
        );
    }

    #[test]
    fn unknown_ids_and_undocumented_suffixes_do_not_guess_capabilities() {
        for model in ["gpt-4o", "gpt-5.4-unverified", "GPT-5.4", "other-model"] {
            assert_eq!(
                resolve(
                    ReasoningSurface::PublicApi,
                    ReasoningApi::Responses,
                    model,
                    ThinkingLevel::High,
                    false
                ),
                Err(UnmappedReason::UnknownModel)
            );
        }
    }

    #[test]
    fn capability_sets_are_ordered_nonempty_unique_and_contain_only_wire_values() {
        for models in [PUBLIC_MODELS, SUBSCRIPTION_MODELS] {
            let mut ids = HashSet::new();
            for entry in models {
                assert!(!entry.models.is_empty());
                assert!(!entry.efforts.is_empty());
                assert!(entry.efforts.windows(2).all(|pair| pair[0] < pair[1]));
                for model in entry.models {
                    assert!(ids.insert(*model), "duplicate model {model}");
                }
                if let Some(tools) = entry.chat_tools {
                    assert!(entry.chat_supported);
                    assert!(!tools.is_empty());
                    assert!(tools.windows(2).all(|pair| pair[0] < pair[1]));
                    assert!(tools.iter().all(|effort| entry.efforts.contains(effort)));
                }
                assert!(
                    entry
                        .efforts
                        .iter()
                        .all(|effort| !matches!(effort.as_str(), "ultra" | "persistent"))
                );
            }
        }
    }

    #[test]
    fn profile_sampling_compatibility_preserves_unmapped_and_none_controls() {
        assert_eq!(profile_temperature(Some(0.7), None), Some(0.7));
        assert_eq!(
            profile_temperature(Some(0.7), Some(NativeEffort::None)),
            Some(0.7)
        );
        assert_eq!(
            profile_temperature(Some(0.7), Some(NativeEffort::High)),
            None
        );
    }

    use crate::traits::{ChatMessage, ChatRequest, ModelProvider, StreamOptions};
    use axum::{Json, Router, response::IntoResponse, routing::post};
    use futures_util::StreamExt;
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};
    use zeroclaw_api::tool::ToolSpec;

    struct CapturedOpenAi {
        url: String,
        requests: Arc<Mutex<Vec<Value>>>,
        server: tokio::task::JoinHandle<()>,
        _proxy_guard: crate::RuntimeProxyTestGuard,
    }

    impl Drop for CapturedOpenAi {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    impl CapturedOpenAi {
        async fn start() -> Self {
            Self::start_rejecting(None).await
        }

        async fn start_rejecting(rejected_model: Option<&'static str>) -> Self {
            let proxy_guard = crate::RuntimeProxyTestGuard::acquire().await;
            let requests = Arc::new(Mutex::new(Vec::new()));
            let response_capture = Arc::clone(&requests);
            let chat_capture = Arc::clone(&requests);
            let anthropic_capture = Arc::clone(&requests);
            let app = Router::new()
            .route(
                "/responses",
                post(move |Json(body): Json<Value>| {
                    let requests = Arc::clone(&response_capture);
                    async move {
                        requests.lock().unwrap().push(body.clone());
                        if rejected_model.is_some_and(|model| body["model"] == model) {
                            return (
                                axum::http::StatusCode::BAD_GATEWAY,
                                "fixture primary unavailable",
                            )
                                .into_response();
                        }
                        let response = json!({
                            "id": "response_fixture", "status": "completed",
                            "output": [{"type": "message", "role": "assistant", "content": [
                                {"type": "output_text", "text": "complete"}
                            ]}],
                            "usage": {"input_tokens": 3, "output_tokens": 1}
                        });
                        if body["stream"] == true {
                            let delta =
                                json!({"type": "response.output_text.delta", "delta": "complete"});
                            let completed =
                                json!({"type": "response.completed", "response": response});
                            (
                                [("content-type", "text/event-stream")],
                                format!("data: {delta}\n\ndata: {completed}\n\n"),
                            )
                                .into_response()
                        } else {
                            Json(response).into_response()
                        }
                    }
                }),
            )
            .route(
                "/chat/completions",
                post(move |Json(body): Json<Value>| {
                    let requests = Arc::clone(&chat_capture);
                    async move {
                        requests.lock().unwrap().push(body);
                        Json(json!({
                            "choices": [{"message": {"role": "assistant", "content": "complete"}}],
                            "usage": {"prompt_tokens": 3, "completion_tokens": 1}
                        }))
                    }
                }),
            )
            .route(
                "/v1/messages",
                post(move |Json(body): Json<Value>| {
                    let requests = Arc::clone(&anthropic_capture);
                    async move {
                        requests.lock().unwrap().push(body);
                        Json(json!({
                            "id": "message_fixture", "type": "message", "role": "assistant",
                            "content": [{"type": "text", "text": "complete"}],
                            "stop_reason": "end_turn",
                            "usage": {"input_tokens": 3, "output_tokens": 1}
                        }))
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = zeroclaw_spawn::spawn!(async move {
                axum::serve(listener, app).await.unwrap();
            });
            Self {
                url,
                requests,
                server,
                _proxy_guard: proxy_guard,
            }
        }

        fn bodies(&self) -> Vec<Value> {
            self.requests.lock().unwrap().clone()
        }

        fn provider(&self, wire: zeroclaw_config::schema::WireApi) -> Box<dyn ModelProvider> {
            let mut config = zeroclaw_config::schema::Config::default();
            config.providers.models.openai.insert(
                "shared".into(),
                zeroclaw_config::schema::OpenAIModelProviderConfig {
                    base: zeroclaw_config::schema::ModelProviderConfig {
                        wire_api: Some(wire),
                        ..Default::default()
                    },
                },
            );
            crate::create_model_provider_for_alias_with_url(
                &config,
                "openai",
                "shared",
                Some("fixture-key"),
                Some(&self.url),
                &crate::ModelProviderRuntimeOptions {
                    reasoning_effort: Some("medium".into()),
                    ..Default::default()
                },
            )
            .unwrap()
        }
    }

    fn tool_fixture() -> Vec<ToolSpec> {
        vec![ToolSpec::new(
            "inspect_value",
            "Inspect one value",
            json!({"type": "object", "properties": {}}),
        )]
    }

    #[tokio::test]
    async fn shared_openai_endpoint_honors_profile_levels_over_conflicting_global_effort() {
        let server = CapturedOpenAi::start().await;
        let provider = server.provider(zeroclaw_config::schema::WireApi::Responses);
        let messages = [ChatMessage::user("inspect")];
        let tools = tool_fixture();
        for level in [ThinkingLevel::Low, ThinkingLevel::High] {
            let profile = zeroclaw_config::schema::RuntimeProfileConfig {
                thinking: zeroclaw_config::scattered_types::ThinkingConfig {
                    native_thinking: true,
                    default_level: level,
                    ..Default::default()
                },
                ..Default::default()
            };
            provider
                .chat(
                    ChatRequest {
                        messages: &messages,
                        tools: Some(&tools),
                        thinking: None,
                        thinking_level: profile
                            .thinking
                            .native_thinking
                            .then_some(profile.thinking.default_level),
                    },
                    "gpt-5.4",
                    Some(0.7),
                )
                .await
                .unwrap();
        }
        provider
            .chat(
                ChatRequest {
                    messages: &messages,
                    tools: Some(&tools),
                    thinking: None,
                    thinking_level: None,
                },
                "gpt-5.4",
                Some(0.7),
            )
            .await
            .unwrap();
        for level in [ThinkingLevel::Low, ThinkingLevel::High] {
            let config = zeroclaw_config::scattered_types::ThinkingConfig {
                native_thinking: true,
                default_level: level,
                ..Default::default()
            };
            provider
                .chat(
                    ChatRequest {
                        messages: &messages,
                        tools: Some(&tools),
                        thinking: config.budget_tokens_for(level).map(|budget_tokens| {
                            zeroclaw_api::model_provider::NativeThinkingParams {
                                budget_tokens,
                                display: None,
                            }
                        }),
                        thinking_level: None,
                    },
                    "gpt-5.4",
                    Some(0.7),
                )
                .await
                .unwrap();
        }
        let bodies = server.bodies();
        assert_eq!(bodies[0]["reasoning"]["effort"], "low");
        assert_eq!(bodies[1]["reasoning"]["effort"], "high");
        assert_eq!(bodies[2]["reasoning"]["effort"], "medium");
        assert_eq!(
            bodies[2], bodies[3],
            "legacy budget-only low profile retains global effort"
        );
        assert_eq!(
            bodies[2], bodies[4],
            "legacy budget-only high profile reproduces the same global effort"
        );
        assert!(bodies[0].get("temperature").is_none());
        assert_eq!(bodies[2]["temperature"], 0.7);
        assert!(!bodies[0]["tools"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn all_levels_reach_both_wire_formats_and_opt_out_preserves_each_existing_body() {
        let messages = [ChatMessage::user("inspect")];
        let tools = tool_fixture();
        for wire in [
            zeroclaw_config::schema::WireApi::Responses,
            zeroclaw_config::schema::WireApi::ChatCompletions,
        ] {
            let server = CapturedOpenAi::start().await;
            let provider = server.provider(wire);
            for level in [
                ThinkingLevel::Off,
                ThinkingLevel::Minimal,
                ThinkingLevel::Low,
                ThinkingLevel::Medium,
                ThinkingLevel::High,
                ThinkingLevel::Max,
            ] {
                provider
                    .chat(
                        ChatRequest {
                            messages: &messages,
                            tools: Some(&tools),
                            thinking: None,
                            thinking_level: Some(level),
                        },
                        "gpt-5.4",
                        Some(0.7),
                    )
                    .await
                    .unwrap();
            }
            for _ in 0..2 {
                provider
                    .chat(
                        ChatRequest {
                            messages: &messages,
                            tools: Some(&tools),
                            thinking: None,
                            thinking_level: None,
                        },
                        "gpt-5.4",
                        Some(0.7),
                    )
                    .await
                    .unwrap();
            }
            let bodies = server.bodies();
            for (body, expected) in bodies
                .iter()
                .zip(["none", "low", "low", "medium", "high", "xhigh"])
            {
                let effort = if wire == zeroclaw_config::schema::WireApi::Responses {
                    &body["reasoning"]["effort"]
                } else {
                    &body["reasoning_effort"]
                };
                assert_eq!(effort, expected);
            }
            assert_eq!(bodies[6], bodies[7]);
            if wire == zeroclaw_config::schema::WireApi::Responses {
                assert_eq!(bodies[6]["reasoning"]["effort"], "medium");
            } else {
                assert!(bodies[6].get("reasoning_effort").is_none());
            }
            assert_eq!(bodies[6]["temperature"], 0.7);
        }
    }

    #[tokio::test]
    async fn deferred_responses_stream_keeps_the_captured_level_after_its_turn_scope_ends() {
        let server = CapturedOpenAi::start().await;
        let provider = server.provider(zeroclaw_config::schema::WireApi::Responses);
        let messages = [ChatMessage::user("inspect")];
        let tools = tool_fixture();
        let mut stream = zeroclaw_api::NATIVE_THINKING_LEVEL
            .scope(Some(ThinkingLevel::Low), async {
                provider.stream_chat(
                    ChatRequest {
                        messages: &messages,
                        tools: Some(&tools),
                        thinking: None,
                        thinking_level: zeroclaw_api::current_native_thinking_level(),
                    },
                    "gpt-5.4",
                    Some(0.7),
                    StreamOptions::new(true),
                )
            })
            .await;
        assert_eq!(zeroclaw_api::current_native_thinking_level(), None);
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        let bodies = server.bodies();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0]["stream"], true);
        assert_eq!(bodies[0]["reasoning"]["effort"], "low");
        assert!(!bodies[0]["tools"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn legacy_one_shot_calls_read_only_the_delegate_carrier() {
        let messages = [ChatMessage::user("inspect")];
        for wire in [
            zeroclaw_config::schema::WireApi::Responses,
            zeroclaw_config::schema::WireApi::ChatCompletions,
        ] {
            let server = CapturedOpenAi::start().await;
            let provider = server.provider(wire);
            zeroclaw_api::NATIVE_THINKING_LEVEL
                .scope(Some(ThinkingLevel::High), async {
                    provider
                        .chat_with_system(None, "inspect", "gpt-5.4", Some(0.7))
                        .await
                        .unwrap();
                    zeroclaw_api::ONE_SHOT_NATIVE_THINKING_LEVEL
                        .scope(Some(ThinkingLevel::Low), async {
                            provider
                                .chat_with_system(None, "inspect", "gpt-5.4", Some(0.7))
                                .await
                                .unwrap();
                        })
                        .await;
                    provider
                        .chat_with_system(None, "inspect", "gpt-5.4", Some(0.7))
                        .await
                        .unwrap();
                })
                .await;
            let bodies = server.bodies();
            assert_eq!(bodies[0], bodies[2]);
            let effort = if wire == zeroclaw_config::schema::WireApi::Responses {
                &bodies[1]["reasoning"]["effort"]
            } else {
                &bodies[1]["reasoning_effort"]
            };
            assert_eq!(effort, "low");
            assert_eq!(
                bodies[0]["messages"].as_array().map(Vec::len),
                if wire == zeroclaw_config::schema::WireApi::ChatCompletions {
                    Some(messages.len())
                } else {
                    None
                }
            );
        }
    }

    impl CapturedOpenAi {
        fn codex_provider(&self) -> (Box<dyn ModelProvider>, tempfile::TempDir) {
            let state = tempfile::tempdir().unwrap();
            let mut config = zeroclaw_config::schema::Config::default();
            config.providers.models.openai.insert(
                "subscription".into(),
                zeroclaw_config::schema::OpenAIModelProviderConfig {
                    base: zeroclaw_config::schema::ModelProviderConfig {
                        requires_openai_auth: true,
                        wire_api: Some(zeroclaw_config::schema::WireApi::Responses),
                        ..Default::default()
                    },
                },
            );
            let provider = crate::create_model_provider_for_alias_with_url(
                &config,
                "openai",
                "subscription",
                Some("fixture-key"),
                Some(&self.url),
                &crate::ModelProviderRuntimeOptions {
                    provider_api_url: Some(self.url.clone()),
                    zeroclaw_dir: Some(state.path().to_path_buf()),
                    secrets_encrypt: false,
                    reasoning_effort: Some("high".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            (provider, state)
        }
    }

    #[tokio::test]
    async fn codex_wire_uses_verified_production_sets_in_chat_streams_and_one_shot_calls() {
        let server = CapturedOpenAi::start().await;
        let (provider, _state) = server.codex_provider();
        let messages = [ChatMessage::user("inspect")];
        let tools = tool_fixture();
        for level in [
            ThinkingLevel::Off,
            ThinkingLevel::Minimal,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
            ThinkingLevel::Max,
        ] {
            provider
                .chat(
                    ChatRequest {
                        messages: &messages,
                        tools: Some(&tools),
                        thinking: None,
                        thinking_level: Some(level),
                    },
                    "gpt-6.1-sol",
                    None,
                )
                .await
                .unwrap();
        }
        for (model, level) in [
            ("gpt-6.1-sol", None),
            ("unverified-model", Some(ThinkingLevel::Max)),
            ("gpt-5.5", Some(ThinkingLevel::Max)),
        ] {
            provider
                .chat(
                    ChatRequest {
                        messages: &messages,
                        tools: Some(&tools),
                        thinking: None,
                        thinking_level: level,
                    },
                    model,
                    None,
                )
                .await
                .unwrap();
        }
        let mut stream = zeroclaw_api::NATIVE_THINKING_LEVEL
            .scope(Some(ThinkingLevel::Minimal), async {
                provider.stream_chat(
                    ChatRequest {
                        messages: &messages,
                        tools: Some(&tools),
                        thinking: None,
                        thinking_level: zeroclaw_api::current_native_thinking_level(),
                    },
                    "gpt-6.1-sol",
                    None,
                    StreamOptions::new(true),
                )
            })
            .await;
        assert_eq!(zeroclaw_api::current_native_thinking_level(), None);
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        zeroclaw_api::NATIVE_THINKING_LEVEL
            .scope(Some(ThinkingLevel::Max), async {
                provider
                    .chat_with_system(None, "inspect", "gpt-6.1-sol", None)
                    .await
                    .unwrap();
                zeroclaw_api::ONE_SHOT_NATIVE_THINKING_LEVEL
                    .scope(Some(ThinkingLevel::Low), async {
                        provider
                            .chat_with_system(None, "inspect", "gpt-6.1-sol", None)
                            .await
                            .unwrap();
                    })
                    .await;
                provider
                    .chat_with_system(None, "inspect", "gpt-6.1-sol", None)
                    .await
                    .unwrap();
            })
            .await;
        let bodies = server.bodies();
        assert_eq!(bodies.len(), 13);
        for (body, expected) in bodies.iter().zip([
            "low", "low", "low", "medium", "high", "max", "high", "high", "xhigh", "low", "high",
            "low", "high",
        ]) {
            assert_eq!(body["reasoning"]["effort"], expected);
            assert_ne!(body["reasoning"]["effort"], "ultra");
        }
        assert!(!bodies[9]["tools"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn legacy_codex_factories_map_native_levels_in_turn_stream_and_one_shot_payloads() {
        for name in ["openai-codex", "openai_codex", "codex"] {
            let server = CapturedOpenAi::start().await;
            let state = tempfile::tempdir().unwrap();
            let provider = crate::create_model_provider_with_options(
                name,
                Some("fixture-key"),
                &crate::ModelProviderRuntimeOptions {
                    provider_api_url: Some(server.url.clone()),
                    zeroclaw_dir: Some(state.path().to_path_buf()),
                    secrets_encrypt: false,
                    reasoning_effort: Some("high".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            let messages = [ChatMessage::user("inspect")];
            let tools = tool_fixture();
            for (model, level) in [
                ("gpt-6.1-sol", Some(ThinkingLevel::Low)),
                ("gpt-6.1-sol", None),
                ("gpt-6.1-sol", Some(ThinkingLevel::Max)),
                ("unverified-model", Some(ThinkingLevel::Max)),
            ] {
                provider
                    .chat(
                        ChatRequest {
                            messages: &messages,
                            tools: Some(&tools),
                            thinking: None,
                            thinking_level: level,
                        },
                        model,
                        None,
                    )
                    .await
                    .unwrap();
            }
            let mut stream = provider.stream_chat(
                ChatRequest {
                    messages: &messages,
                    tools: Some(&tools),
                    thinking: None,
                    thinking_level: Some(ThinkingLevel::Minimal),
                },
                "gpt-6.1-sol",
                None,
                StreamOptions::new(true),
            );
            while let Some(event) = stream.next().await {
                event.unwrap();
            }
            zeroclaw_api::ONE_SHOT_NATIVE_THINKING_LEVEL
                .scope(Some(ThinkingLevel::Off), async {
                    provider
                        .chat_with_system(None, "inspect", "gpt-6.1-sol", None)
                        .await
                        .unwrap();
                })
                .await;
            let bodies = server.bodies();
            assert_eq!(bodies.len(), 6, "{name}");
            for (body, expected) in bodies
                .iter()
                .zip(["low", "high", "max", "high", "low", "low"])
            {
                assert_eq!(body["reasoning"]["effort"], expected, "{name}");
            }
            assert!(!bodies[4]["tools"].as_array().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn the_same_model_uses_distinct_public_and_subscription_wire_capabilities() {
        let server = CapturedOpenAi::start().await;
        let public = server.provider(zeroclaw_config::schema::WireApi::Responses);
        let (subscription, _state) = server.codex_provider();
        let messages = [ChatMessage::user("inspect")];
        for provider in [public, subscription] {
            provider
                .chat(
                    ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                        thinking_level: Some(ThinkingLevel::Off),
                    },
                    "gpt-6.1-terra",
                    None,
                )
                .await
                .unwrap();
        }
        let bodies = server.bodies();
        assert_eq!(bodies[0]["reasoning"]["effort"], "none");
        assert_eq!(bodies[1]["reasoning"]["effort"], "low");
    }

    #[tokio::test]
    async fn shared_response_and_chat_leaves_do_not_enable_other_provider_families() {
        let server = CapturedOpenAi::start().await;
        let config: zeroclaw_config::schema::Config = serde_json::from_value(serde_json::json!({
            "providers": {"models": {"custom": {"foreign": {"wire_api": "responses"}}}}
        }))
        .unwrap();
        let foreign_responses = crate::create_model_provider_for_alias_with_url(
            &config,
            "custom",
            "foreign",
            Some("fixture-key"),
            Some(&server.url),
            &crate::ModelProviderRuntimeOptions {
                reasoning_effort: Some("medium".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let foreign_chat = crate::openai::OpenAiModelProvider::builder("openai")
            .credential(Some("fixture-key"))
            .base_url(&server.url)
            .build();
        let messages = [ChatMessage::user("inspect")];
        for level in [None, Some(ThinkingLevel::Max)] {
            foreign_responses
                .chat(
                    ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                        thinking_level: level,
                    },
                    "gpt-5.4",
                    Some(0.7),
                )
                .await
                .unwrap();
            foreign_chat
                .chat(
                    ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                        thinking_level: level,
                    },
                    "gpt-5.4",
                    Some(0.7),
                )
                .await
                .unwrap();
        }
        let bodies = server.bodies();
        assert_eq!(bodies[0], bodies[2]);
        assert_eq!(bodies[1], bodies[3]);
        assert_eq!(bodies[2]["reasoning"]["effort"], "medium");
        assert!(bodies[3].get("reasoning_effort").is_none());
        assert_eq!(bodies[2]["temperature"], 0.7);
        assert_eq!(bodies[3]["temperature"], 0.7);
    }

    #[tokio::test]
    async fn actual_openai_model_fallbacks_remap_the_level_on_chat_and_stream_recovery() {
        let server = CapturedOpenAi::start_rejecting(Some("gpt-5.4")).await;
        let messages = [ChatMessage::user("inspect")];
        let provider: Arc<dyn ModelProvider> =
            Arc::from(server.provider(zeroclaw_config::schema::WireApi::Responses));
        for streaming in [false, true] {
            let reliable = crate::reliable::ReliableModelProvider::new_pinned_for_test(
                "reliable",
                vec![
                    ("openai.shared", "shared", "gpt-5.4", Arc::clone(&provider)),
                    (
                        "openai.shared",
                        "shared",
                        "gpt-6.1-sol",
                        Arc::clone(&provider),
                    ),
                ],
                0,
                1,
            );
            let accounting = crate::dispatch::AccountedChatScope::new();
            let served = accounting
                .scope(crate::reliable::scope_provider_fallback(async {
                    let request = ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                        thinking_level: Some(ThinkingLevel::Off),
                    };
                    if streaming {
                        let mut stream = reliable.stream_chat(
                            request,
                            "gpt-5.4",
                            Some(0.7),
                            StreamOptions::new(true),
                        );
                        let error = stream.next().await.unwrap().unwrap_err();
                        drop(stream);
                        accounting.record_stream_recovery_failure(&anyhow::Error::new(error));
                    }
                    let response = reliable.chat(request, "gpt-5.4", Some(0.7)).await.unwrap();
                    assert_eq!(response.text.as_deref(), Some("complete"));
                    assert_eq!(response.usage.unwrap().input_tokens, Some(3));
                    accounting.mark_logical_success();
                    crate::reliable::take_last_provider_fallback().unwrap()
                }))
                .await;
            assert_eq!(served.requested_model, "gpt-5.4");
            assert_eq!(served.actual_model, "gpt-6.1-sol");
            let report = accounting.take();
            assert_eq!(
                report.attempts().len(),
                2,
                "failed stream must not replay the primary"
            );
            assert_eq!(report.accepted_route().unwrap().model(), "gpt-6.1-sol");
            assert!(report.rejected_attempts().is_empty());
            let crate::dispatch::AttemptUsageOutcome::Complete(usage) =
                report.attempts()[1].outcome()
            else {
                panic!("accepted fallback must retain complete usage");
            };
            assert_eq!(usage.input_tokens, Some(3));
        }
        let bodies = server.bodies();
        assert_eq!(bodies.len(), 4);
        for pair in bodies.as_chunks::<2>().0 {
            assert_eq!(pair[0]["model"], "gpt-5.4");
            assert_eq!(pair[0]["reasoning"]["effort"], "none");
            assert_eq!(pair[1]["model"], "gpt-6.1-sol");
            assert_eq!(pair[1]["reasoning"]["effort"], "low");
        }
    }

    #[tokio::test]
    async fn mixed_openai_anthropic_fallback_preserves_the_budget_and_foreign_wire_body() {
        let server = CapturedOpenAi::start_rejecting(Some("gpt-5.4")).await;
        let anthropic: Arc<dyn ModelProvider> = Arc::new(
            crate::anthropic::AnthropicModelProvider::builder("target")
                .credential(Some("fixture-key"))
                .base_url(&server.url)
                .build(),
        );
        let messages = [ChatMessage::user("inspect")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            thinking: Some(zeroclaw_api::model_provider::NativeThinkingParams {
                budget_tokens: 2_048,
                display: None,
            }),
            thinking_level: None,
        };
        anthropic
            .chat(request, "claude-sonnet-4-6", Some(0.7))
            .await
            .unwrap();
        let openai: Arc<dyn ModelProvider> =
            Arc::from(server.provider(zeroclaw_config::schema::WireApi::Responses));
        let reliable = crate::reliable::ReliableModelProvider::new_pinned_for_test(
            "mixed",
            vec![
                ("openai.shared", "shared", "gpt-5.4", openai),
                ("anthropic.target", "target", "claude-sonnet-4-6", anthropic),
            ],
            0,
            1,
        );
        let result = reliable
            .chat(
                ChatRequest {
                    thinking_level: Some(ThinkingLevel::High),
                    ..request
                },
                "gpt-5.4",
                Some(0.7),
            )
            .await
            .unwrap();
        assert_eq!(result.text.as_deref(), Some("complete"));
        let bodies = server.bodies();
        assert_eq!(bodies.len(), 3);
        assert_eq!(bodies[0], bodies[2]);
        assert_eq!(bodies[2]["thinking"]["budget_tokens"], 2_048);
        assert!(bodies[2].get("reasoning").is_none());
        assert!(bodies[2].get("reasoning_effort").is_none());
        assert_eq!(bodies[1]["reasoning"]["effort"], "high");
    }

    #[tokio::test]
    async fn router_synthesized_chat_stream_preserves_the_captured_native_level() {
        let server = CapturedOpenAi::start().await;
        let router = crate::router::RouterModelProvider::new(
            "native-router",
            vec![(
                "openai.shared".into(),
                server.provider(zeroclaw_config::schema::WireApi::ChatCompletions),
            )],
            Vec::new(),
            "gpt-5.4".into(),
        );
        let messages = [ChatMessage::user("inspect")];
        let mut stream = zeroclaw_api::NATIVE_THINKING_LEVEL
            .scope(Some(ThinkingLevel::Low), async {
                router.stream_chat(
                    ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                        thinking_level: zeroclaw_api::current_native_thinking_level(),
                    },
                    "gpt-5.4",
                    Some(0.7),
                    StreamOptions::new(true),
                )
            })
            .await;
        assert_eq!(zeroclaw_api::current_native_thinking_level(), None);
        assert!(
            server.bodies().is_empty(),
            "synthesized stream must remain lazy"
        );
        let mut final_seen = false;
        while let Some(event) = stream.next().await {
            final_seen |= matches!(event.unwrap(), crate::traits::StreamEvent::Final);
        }
        assert!(final_seen);
        let bodies = server.bodies();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0]["reasoning_effort"], "low");
    }

    #[tokio::test]
    async fn unmapped_public_requests_preserve_the_existing_wire_body() {
        for wire in [
            zeroclaw_config::schema::WireApi::Responses,
            zeroclaw_config::schema::WireApi::ChatCompletions,
        ] {
            let server = CapturedOpenAi::start().await;
            let provider = server.provider(wire);
            let messages = [ChatMessage::user("inspect")];
            for model in ["gpt-4o", "gpt-5.4-unverified", "GPT-5.4"] {
                for level in [None, Some(ThinkingLevel::Max)] {
                    provider
                        .chat(
                            ChatRequest {
                                messages: &messages,
                                tools: None,
                                thinking: None,
                                thinking_level: level,
                            },
                            model,
                            Some(0.7),
                        )
                        .await
                        .unwrap();
                }
            }
            if matches!(wire, zeroclaw_config::schema::WireApi::ChatCompletions) {
                let tools = tool_fixture();
                for level in [None, Some(ThinkingLevel::High)] {
                    provider
                        .chat(
                            ChatRequest {
                                messages: &messages,
                                tools: Some(&tools),
                                thinking: None,
                                thinking_level: level,
                            },
                            "gpt-6.1-sol",
                            Some(0.7),
                        )
                        .await
                        .unwrap();
                }
            }
            for pair in server.bodies().as_chunks::<2>().0 {
                assert_eq!(pair[0], pair[1]);
            }
        }
    }

    #[test]
    fn reasoning_diagnostics_explain_substitution_and_unmapped_levels_without_request_data() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        struct RestoreHook(Option<zeroclaw_log::LogBroadcastSender>);
        impl Drop for RestoreHook {
            fn drop(&mut self) {
                if let Some(previous) = self.0.take() {
                    zeroclaw_log::set_broadcast_hook(previous);
                } else {
                    zeroclaw_log::clear_broadcast_hook();
                }
            }
        }
        let _restore = RestoreHook(zeroclaw_log::current_broadcast_hook());
        let (tx, mut rx) = tokio::sync::broadcast::channel(256);
        zeroclaw_log::set_broadcast_hook(tx);
        let context = ReasoningContext {
            surface: Some(ReasoningSurface::PublicApi),
            api: ReasoningApi::Responses,
            alias: "native-diagnostic",
            model: "unverified-model",
            has_tools: false,
            custom_endpoint: true,
            call: ReasoningCall::Turn,
        };
        assert_eq!(
            context.resolve_level(Some(ThinkingLevel::Max), Some("medium"), Some("medium")),
            None
        );
        let mapped = ReasoningContext {
            model: "gpt-5.4",
            ..context
        };
        assert_eq!(
            mapped.resolve_level(Some(ThinkingLevel::Minimal), Some("medium"), Some("medium")),
            Some(NativeEffort::Low)
        );
        assert_eq!(
            mapped.resolve_level(Some(ThinkingLevel::High), None, None),
            Some(NativeEffort::High)
        );
        assert_eq!(mapped.resolve_level(None, None, None), None);
        let mut records = Vec::new();
        while let Ok(event) = rx.try_recv() {
            let attrs = &event["attributes"];
            if attrs["alias"] == "native-diagnostic" {
                records.push(attrs.clone());
            }
        }
        assert_eq!(
            records.len(),
            if zeroclaw_log::debug_enabled() { 3 } else { 1 }
        );
        let unknown = &records[0];
        assert_eq!(unknown["requested_level"], "max");
        assert_eq!(unknown["effective_effort"], "medium");
        assert_eq!(unknown["model"], "unverified-model");
        assert_eq!(unknown["outcome"], "unmapped");
        assert_eq!(unknown["unmapped_reason"], "unknown_model");
        assert_eq!(unknown["global_effort_overridden"], false);
        if records.len() == 3 {
            assert_eq!(records[1]["outcome"], "substituted");
            assert_eq!(records[1]["effective_effort"], "low");
            assert_eq!(records[1]["global_effort_overridden"], true);
            assert_eq!(records[2]["outcome"], "exact");
        }
        for attrs in records {
            assert_eq!(attrs["provider"], "openai");
            assert_eq!(attrs["request_api"], "responses");
            assert_eq!(attrs["surface"], "public_api");
            assert_eq!(attrs["custom_endpoint"], true);
            assert_eq!(attrs["call"], "turn");
            for key in [
                "url",
                "endpoint",
                "headers",
                "messages",
                "tools",
                "credentials",
                "api_key",
            ] {
                assert!(attrs.get(key).is_none());
            }
        }
    }

    #[tokio::test]
    async fn single_call_reliable_fallback_remaps_the_scoped_delegate_level() {
        let server = CapturedOpenAi::start_rejecting(Some("gpt-5.4")).await;
        let provider: Arc<dyn ModelProvider> =
            Arc::from(server.provider(zeroclaw_config::schema::WireApi::Responses));
        let reliable = crate::reliable::ReliableModelProvider::new_pinned_for_test(
            "single-call",
            vec![
                ("openai.shared", "shared", "gpt-5.4", Arc::clone(&provider)),
                ("openai.shared", "shared", "gpt-6.1-sol", provider),
            ],
            0,
            1,
        );
        let served = crate::reliable::scope_provider_fallback(async {
            let answer = zeroclaw_api::ONE_SHOT_NATIVE_THINKING_LEVEL
                .scope(
                    Some(ThinkingLevel::Off),
                    reliable.chat_with_system(
                        Some("inspect carefully"),
                        "inspect",
                        "gpt-5.4",
                        Some(0.7),
                    ),
                )
                .await
                .unwrap();
            assert_eq!(answer, "complete");
            crate::reliable::take_last_provider_fallback().unwrap()
        })
        .await;
        assert_eq!(served.actual_model, "gpt-6.1-sol");
        let bodies = server.bodies();
        assert_eq!(bodies.len(), 2);
        assert_eq!(bodies[0]["reasoning"]["effort"], "none");
        assert_eq!(bodies[1]["reasoning"]["effort"], "low");
    }
}
