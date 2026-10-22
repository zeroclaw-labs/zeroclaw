//! Call-scoped sampling policy. Config owns defaults; callers own overrides.
use std::sync::Arc;
use zeroclaw_api::model_provider::CallSampling;
use zeroclaw_config::schema::Config;

/// Invocation input, kept separate from any configuration-derived default.
#[derive(Clone, Copy, Debug, Default)]
pub struct Temperature {
    pub override_value: Option<f64>,
    pub adjustment: f64,
}

impl From<Option<f64>> for Temperature {
    fn from(override_value: Option<f64>) -> Self {
        Self {
            override_value,
            adjustment: 0.0,
        }
    }
}

impl Temperature {
    pub fn adjusted(override_value: Option<f64>, adjustment: f64) -> Self {
        Self {
            override_value,
            adjustment,
        }
    }

    pub fn resolve(self, config: Option<&Config>, selection_ref: &str) -> Option<f64> {
        let configured = config
            .and_then(|config| config.resolve_model_selection(selection_ref))
            .and_then(|selection| selection.temperature());
        self.override_value.or(configured).map(|value| {
            if self.adjustment == 0.0 {
                value
            } else {
                (value + self.adjustment).clamp(0.0, 2.0)
            }
        })
    }

    /// Resolve configured defaults explicitly for this call's actual candidate.
    /// `None` invocation input inherits configuration; use
    /// `CallSampling::Explicit(None)` to omit sampling regardless of config.
    pub fn for_call(self, config: Option<Arc<Config>>, selection_ref: &str) -> CallSampling {
        let default = self.resolve(config.as_deref(), selection_ref);
        self.for_call_with_fallback(config, selection_ref, default)
    }

    /// Use an already-resolved entrance value for unknown references, including
    /// fallback candidates. Known selections retain their own defaults, even
    /// when unset. The supplied fallback already includes adjustment.
    pub fn for_call_with_fallback(
        self,
        config: Option<Arc<Config>>,
        selection_ref: &str,
        fallback: Option<f64>,
    ) -> CallSampling {
        let default = if config
            .as_deref()
            .and_then(|config| config.resolve_model_selection(selection_ref))
            .is_some()
        {
            self.resolve(config.as_deref(), selection_ref)
        } else {
            fallback
        };
        let Some(config) = config else {
            return CallSampling::Explicit(default);
        };
        CallSampling::for_selection(default, move |candidate| {
            if config.resolve_model_selection(candidate).is_some() {
                self.resolve(Some(&config), candidate)
            } else {
                fallback
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelProvider;
    use futures_util::{StreamExt, stream};
    use zeroclaw_api::model_provider::{
        ChatMessage, ChatRequest, ChatResponse, StreamChunk, StreamEvent, StreamOptions,
        StreamResult,
    };
    use zeroclaw_config::schema::ModelEntryConfig;

    type CallLog = Arc<std::sync::Mutex<Vec<(&'static str, Option<f64>)>>>;

    struct SamplingRecorder {
        calls: CallLog,
        fail: bool,
        streaming: bool,
    }

    impl SamplingRecorder {
        fn record(&self, method: &'static str, temperature: Option<f64>) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push((method, temperature));
            if self.fail {
                anyhow::bail!("401 fixture primary unavailable");
            }
            Ok(())
        }
    }

    impl zeroclaw_api::attribution::Attributable for SamplingRecorder {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            use zeroclaw_api::attribution::{ModelProviderKind, ProviderKind, Role};
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }
        fn alias(&self) -> &str {
            "recorder"
        }
    }

    #[async_trait::async_trait]
    impl ModelProvider for SamplingRecorder {
        fn supports_streaming(&self) -> bool {
            self.streaming
        }
        fn supports_streaming_tool_events(&self) -> bool {
            self.streaming
        }
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            self.record("chat_with_system", temperature)?;
            Ok("ok".into())
        }

        async fn chat_with_history(
            &self,
            _messages: &[ChatMessage],
            _model: &str,
            temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            self.record("chat_with_history", temperature)?;
            Ok("ok".into())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            self.record("chat", temperature)?;
            Ok(ChatResponse {
                text: Some("ok".into()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }

        async fn chat_with_tools(
            &self,
            _messages: &[ChatMessage],
            _tools: &[serde_json::Value],
            _model: &str,
            temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            self.record("chat_with_tools", temperature)?;
            Ok(ChatResponse {
                text: Some("ok".into()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }

        fn stream_chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            temperature: Option<f64>,
            _options: StreamOptions,
        ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
            self.record("stream_chat_with_system", temperature).unwrap();
            futures_util::stream::iter(vec![Ok(StreamChunk::final_chunk())]).boxed()
        }

        fn stream_chat_with_history(
            &self,
            _messages: &[ChatMessage],
            _model: &str,
            temperature: Option<f64>,
            _options: StreamOptions,
        ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
            self.record("stream_chat_with_history", temperature)
                .unwrap();
            futures_util::stream::iter(vec![Ok(StreamChunk::final_chunk())]).boxed()
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            temperature: Option<f64>,
            _options: StreamOptions,
        ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
            self.record("stream_chat", temperature).unwrap();
            futures_util::stream::iter(vec![Ok(StreamEvent::Final)]).boxed()
        }
    }

    #[tokio::test]
    async fn explicit_sampling_survives_router_pin_fallback_and_stream_polling() {
        use crate::reliable::{ReliableModelProvider, ReliableModelProviderEntry};
        use crate::router::{Route, RouterModelProvider};
        let calls = CallLog::default();
        let entries = [("primary", "fast", true), ("backup", "default", false)]
            .into_iter()
            .map(|(alias, entry, fail)| {
                let leaf = crate::vision_override::VisionOverrideProvider::new(
                    Box::new(SamplingRecorder {
                        calls: Arc::clone(&calls),
                        fail,
                        streaming: !fail,
                    }),
                    true,
                );
                ReliableModelProviderEntry::new_pinned(
                    "custom",
                    format!("custom.{alias}"),
                    alias,
                    "model-id",
                    Box::new(leaf),
                )
                .with_selection_ref(&format!("custom.{alias}.{entry}"))
            })
            .collect();
        let reliable = ReliableModelProvider::new_with_entries("primary", entries, 0, 0);
        let router = RouterModelProvider::new(
            "router",
            vec![("custom.primary.fast".into(), Box::new(reliable))],
            vec![(
                "fast".into(),
                Route {
                    provider_name: "custom.primary.fast".into(),
                    model: "model-id".into(),
                },
            )],
            "model-id".into(),
        );
        let mut config = Config::default();
        config
            .providers
            .models
            .ensure("custom", "entrance")
            .unwrap()
            .temperature = Some(0.9);
        for (alias, entry, temperature) in [("primary", "fast", 0.2), ("backup", "default", 0.8)] {
            config
                .providers
                .models
                .ensure("custom", alias)
                .unwrap()
                .models
                .insert(
                    entry.into(),
                    ModelEntryConfig {
                        temperature: Some(temperature),
                        ..Default::default()
                    },
                );
        }
        let configured = Temperature::default().for_call(Some(Arc::new(config)), "custom.entrance");
        let dispatch = crate::ProviderDispatch::from_ref(&router);
        let messages = [ChatMessage::user("hello")];
        for (sampling, expected) in [
            (configured.clone(), vec![Some(0.2), Some(0.8)]),
            (
                CallSampling::Explicit(Some(0.4)),
                vec![Some(0.4), Some(0.4)],
            ),
            (CallSampling::Explicit(None), vec![None, None]),
        ] {
            dispatch
                .chat_with_system_with_sampling(None, "hello", "hint:fast", sampling.clone())
                .await
                .unwrap();
            dispatch
                .chat_with_history_with_sampling(&messages, "hint:fast", sampling.clone())
                .await
                .unwrap();
            dispatch
                .chat_with_sampling(ChatRequest::new(&messages), "hint:fast", sampling.clone())
                .await
                .unwrap();
            dispatch
                .chat_with_tools_with_sampling(&messages, &[], "hint:fast", sampling)
                .await
                .unwrap();
            let actual = std::mem::take(&mut *calls.lock().unwrap());
            for (attempts, method) in actual.as_chunks::<2>().0.iter().zip([
                "chat_with_system",
                "chat_with_history",
                "chat",
                "chat_with_tools",
            ]) {
                assert_eq!(
                    attempts.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
                    vec![method; 2]
                );
                assert_eq!(
                    attempts
                        .iter()
                        .map(|(_, temperature)| *temperature)
                        .collect::<Vec<_>>(),
                    expected
                );
            }
            assert_eq!(actual.len(), 8);
        }
        // The legacy scalar contract stays independent of configured calls.
        dispatch
            .chat_with_system(None, "hello", "hint:fast", None)
            .await
            .unwrap();
        assert!(
            std::mem::take(&mut *calls.lock().unwrap())
                .iter()
                .all(|(_, temperature)| temperature.is_none())
        );

        dispatch
            .stream_chat_with_system_with_sampling(
                None,
                "hello",
                "hint:fast",
                configured.clone(),
                StreamOptions::new(true),
            )
            .collect::<Vec<_>>()
            .await;
        dispatch
            .stream_chat_with_history_with_sampling(
                &messages,
                "hint:fast",
                configured.clone(),
                StreamOptions::new(true),
            )
            .collect::<Vec<_>>()
            .await;
        let stream = dispatch.stream_chat_with_sampling(
            ChatRequest::new(&messages),
            "hint:fast",
            configured.clone(),
            StreamOptions::new(true),
        );
        ::zeroclaw_spawn::spawn!(async move { stream.collect::<Vec<_>>().await })
            .await
            .unwrap();
        let actual = std::mem::take(&mut *calls.lock().unwrap());
        assert_eq!(
            actual,
            vec![
                ("stream_chat_with_system", Some(0.8)),
                ("stream_chat_with_history", Some(0.8)),
                ("stream_chat", Some(0.8)),
            ]
        );

        let synthesized = RouterModelProvider::new(
            "synthesized",
            vec![(
                "custom.backup.default".into(),
                Box::new(SamplingRecorder {
                    calls: Arc::clone(&calls),
                    fail: false,
                    streaming: false,
                }),
            )],
            Vec::new(),
            "model-id".into(),
        );
        let stream = crate::ProviderDispatch::from_ref(&synthesized).stream_chat_with_sampling(
            ChatRequest::new(&messages),
            "model-id",
            configured,
            StreamOptions::new(true),
        );
        ::zeroclaw_spawn::spawn!(async move { stream.collect::<Vec<_>>().await })
            .await
            .unwrap();
        assert_eq!(*calls.lock().unwrap(), vec![("chat", Some(0.8))]);
    }

    #[test]
    fn sampling_resolves_target_then_adjusts_without_ambient_state() {
        let mut config = Config::default();
        let profile = config.providers.models.ensure("custom", "gw").unwrap();
        profile.temperature = Some(0.9);
        profile.models.insert(
            "chosen".into(),
            ModelEntryConfig {
                id: Some("shared-id".into()),
                temperature: Some(0.2),
                ..Default::default()
            },
        );
        config.providers.models.ensure("custom", "unset").unwrap();
        let config = Arc::new(config);
        for (input, expected) in [
            (Temperature::adjusted(None, 0.1), 0.3),
            (Temperature::adjusted(Some(0.2), 0.1), 0.3),
            (Temperature::adjusted(Some(1.9), 0.5), 2.0),
        ] {
            let sampling = input.for_call(Some(Arc::clone(&config)), "custom.gw.chosen");
            assert!((sampling.resolve_for(None).unwrap() - expected).abs() < 1e-9);
            assert!(
                (sampling.resolve_for(Some("custom.gw.chosen")).unwrap() - expected).abs() < 1e-9
            );
            assert_eq!(
                sampling.resolve_for(Some("custom.unset")),
                input.resolve(Some(&config), "custom.unset")
            );
            assert_eq!(
                CallSampling::Explicit(None).resolve_for(Some("custom.gw.chosen")),
                None
            );
            assert_eq!(
                CallSampling::Explicit(Some(0.7)).resolve_for(Some("custom.gw.chosen")),
                Some(0.7)
            );
        }
    }

    #[test]
    fn resolved_fallback_is_only_used_for_unknown_selections() {
        let mut config = Config::default();
        config
            .providers
            .models
            .ensure("ollama", "configured")
            .unwrap()
            .temperature = Some(0.2);
        config.providers.models.ensure("ollama", "unset").unwrap();
        let config = Arc::new(config);
        let input = Temperature::adjusted(None, 0.1);
        let unknown = input.for_call_with_fallback(Some(Arc::clone(&config)), "ollama", Some(0.7));
        assert_eq!(unknown.resolve_for(None), Some(0.7));
        assert_eq!(unknown.resolve_for(Some("missing.profile")), Some(0.7));
        assert_eq!(unknown.resolve_for(Some("ollama.unset")), None);
        assert!((unknown.resolve_for(Some("ollama.configured")).unwrap() - 0.3).abs() < 1e-9);

        let unset = input.for_call_with_fallback(Some(config), "ollama.unset", Some(0.7));
        assert_eq!(unset.resolve_for(None), None);
        assert_eq!(unset.resolve_for(Some("missing.profile")), Some(0.7));
        assert_eq!(
            input
                .for_call_with_fallback(None, "ollama", Some(0.7))
                .resolve_for(None),
            Some(0.7)
        );
    }
}
