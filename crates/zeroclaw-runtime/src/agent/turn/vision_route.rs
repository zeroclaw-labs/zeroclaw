//! Vision model-provider routing and per-iteration message preparation.

use anyhow::{Context, Result};
use zeroclaw_config::schema::{Config, MultimodalConfig};
use zeroclaw_providers::{ChatMessage, ModelProvider, ProviderCapabilityError, multimodal};

pub(crate) struct ResolvedVisionProvider {
    pub(crate) provider: Box<dyn ModelProvider>,
    pub(crate) provider_name: String,
    pub(crate) model: String,
}

pub(crate) async fn resolve_vision_provider(
    config: Option<&Config>,
    model_provider: &dyn ModelProvider,
    history: &[ChatMessage],
    multimodal_config: &MultimodalConfig,
    provider_name: &str,
    model: &str,
) -> Result<(Option<ResolvedVisionProvider>, bool)> {
    let image_marker_count = multimodal::count_image_markers(history);
    let latest_user_image_marker_count = multimodal::count_latest_user_image_markers(history);

    let mut degrade_strip_images = false;
    let vision_model_provider: Option<ResolvedVisionProvider> = if image_marker_count > 0 {
        // Resolve the PRIMARY provider's capability atomically (async warm +
        // query in one owner). Unlike a raw `warm_capabilities_metadata` +
        // sync `capabilities_for_model` pair, a catalog failure surfaces as an
        // `Err` instead of silently degrading to the family default — so
        // "capability unknown" is never conflated with an authoritative
        // "no vision". A caller at the image gate must treat that `Err` as
        // unknown, not as a negative result (fnd-001 keeps text-only turns off
        // this path: the catalog request is only made when the turn actually
        // carries image markers).
        match model_provider.resolve_capabilities_for_model(model).await {
            // Primary provider explicitly supports vision: no dedicated route needed.
            Ok(capabilities) if capabilities.vision => None,
            // Authoritative no-vision from the primary provider: try the configured
            // dedicated vision route, else error on latest-user image or degrade
            // carried-over tool-result image to text-only.
            Ok(_) => {
                if let Some(ref vp) = multimodal_config.vision_model_provider {
                    resolve_configured_vision_provider(config, multimodal_config, model, vp).await?
                } else if latest_user_image_marker_count > 0 {
                    return Err(ProviderCapabilityError {
                        model_provider: provider_name.to_string(),
                        capability: "vision".to_string(),
                        message: format!(
                            "received {latest_user_image_marker_count} image marker(s), but this model_provider does not support vision input"
                        ),
                    }
                    .into());
                } else {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_category(::zeroclaw_log::EventCategory::Provider)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "model_provider": provider_name,
                                "image_marker_count": image_marker_count,
                            })),
                        "no vision route for carried-over/tool-result image marker(s); degrading to text-only (markers stripped)"
                    );
                    degrade_strip_images = true;
                    None
                }
            }
            // Capability unknown: the primary catalog could not be resolved. This is
            // NOT evidence the model lacks vision. If a dedicated vision provider is
            // configured, keep it usable by routing to it; otherwise surface the
            // lookup failure rather than converting it into "no vision" or stripping
            // the image.
            Err(primary_err) => {
                if let Some(ref vp) = multimodal_config.vision_model_provider {
                    resolve_configured_vision_provider(config, multimodal_config, model, vp)
                        .await
                        .map_err(|error| {
                        error.context(format!(
                            "primary provider '{provider_name}' capability resolve failed while routing to configured vision provider '{vp}'"
                        ))
                    })?
                } else {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_category(::zeroclaw_log::EventCategory::Provider)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "model_provider": provider_name,
                                "model": model,
                                "image_marker_count": image_marker_count,
                            })),
                        "cannot verify vision capability: model catalog resolve failed; treating as unknown, not as no-vision"
                    );
                    return Err(anyhow::Error::msg(format!(
                        "cannot verify vision capability for model '{model}' on provider '{provider_name}': {primary_err:#} (received {image_marker_count} image marker(s), but the provider's model catalog could not be resolved; treating vision as unknown, not as unsupported)"
                    )));
                }
            }
        }
    } else {
        None
    };

    Ok((vision_model_provider, degrade_strip_images))
}

/// Construct the configured dedicated vision provider and confirm it can take
/// the image. `primary_unknown` carries the primary provider's catalog resolve
/// error when capability is unknown (vs authoritative no-vision); it is not
/// otherwise consulted here — the vision route only needs the configured
/// provider itself to resolve to a vision-capable model. On an authoritative
/// no-vision primary the caller only reaches this for the fallback path.
async fn resolve_configured_vision_provider(
    config: Option<&Config>,
    multimodal_config: &MultimodalConfig,
    model: &str,
    vp: &str,
) -> Result<Option<ResolvedVisionProvider>> {
    // Resolve the configured vision provider through the alias-aware factory so
    // its per-alias `vision` override and typed config (endpoint URI,
    // credentials) are honored - the legacy `create_model_provider(vp, None)`
    // passed `config = None` and could not see them, so a text-family alias
    // forced to `vision = true` for this route would have been ignored. `config`
    // is `None` only on configless (test-builder) agents - every production
    // agent/loop path threads `Some`; that fallback keeps the prior legacy
    // behavior.
    let (vp_instance, alias_model) = match config {
        Some(config) => zeroclaw_providers::create_model_provider_from_ref_with_model(config, vp)
            .map(|resolved| (resolved.provider, resolved.model)),
        None => {
            zeroclaw_providers::create_model_provider(vp, None).map(|provider| (provider, None))
        }
    }
    .map_err(|error| {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_category(::zeroclaw_log::EventCategory::Provider)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "vision_provider": vp,
                    "error": format!("{error}"),
                })),
            "vision model_provider construction failed"
        );
        anyhow::Error::msg(format!(
            "failed to create vision model_provider '{vp}': {error}"
        ))
    })?;
    let vision_model = multimodal_config
        .vision_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToString::to_string)
        .or(alias_model)
        .unwrap_or_else(|| model.to_string());
    // Resolve the vision provider's capability atomically so a model catalog
    // failure on the configured route surfaces as an `Err` (capability unknown)
    // rather than silently falling back to the family default.
    match vp_instance
        .resolve_capabilities_for_model(&vision_model)
        .await
    {
        Ok(capabilities) if capabilities.vision => Ok(Some(ResolvedVisionProvider {
            provider: vp_instance,
            provider_name: vp.to_string(),
            model: vision_model,
        })),
        Ok(_) => Err(ProviderCapabilityError {
            model_provider: vp.to_string(),
            capability: "vision".to_string(),
            message: format!(
                "configured vision_model_provider '{vp}' does not support vision input"
            ),
        }
        .into()),
        Err(err) => Err(err).context(format!(
            "configured vision_model_provider '{vp}' capability resolve failed"
        )),
    }
}

pub(crate) async fn prepare_messages_for_iteration(
    history: &[ChatMessage],
    multimodal_config: &MultimodalConfig,
    degrade_strip_images: bool,
    image_cache: Option<&mut multimodal::LocalImageCache>,
) -> Result<multimodal::PreparedMessages> {
    // Enforce the universal leading-turn-order invariant before any provider
    // sees the history: strict providers reject a first non-system turn that is
    // not `user`, which context trims and session restores can produce.
    let mut sanitized = history.to_vec();
    ChatMessage::sanitize_leading_turn_order(&mut sanitized);
    if !sanitized.iter().any(ChatMessage::is_user) {
        anyhow::bail!(
            "refusing to dispatch to provider: prepared history has no user turn \
             (system-only after leading-turn-order sanitize)"
        );
    }
    let history = sanitized.as_slice();
    if degrade_strip_images {
        // Text-only fallback: replace every media marker with a
        // `[media attachment]` placeholder so no filesystem path or data
        // URI reaches the text-only provider, while surrounding text
        // (captions, tool metadata) survives.
        let stripped: Vec<ChatMessage> = history
            .iter()
            .map(|m| ChatMessage {
                role: m.role.clone(),
                content: multimodal::strip_media_markers(&m.content),
            })
            .collect();
        match image_cache {
            Some(cache) => {
                multimodal::prepare_messages_for_provider_cached(
                    &stripped,
                    multimodal_config,
                    cache,
                )
                .await
            }
            None => multimodal::prepare_messages_for_provider(&stripped, multimodal_config).await,
        }
    } else {
        match image_cache {
            Some(cache) => {
                multimodal::prepare_messages_for_provider_cached(history, multimodal_config, cache)
                    .await
            }
            None => multimodal::prepare_messages_for_provider(history, multimodal_config).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn prepare_messages_for_iteration_populates_and_reuses_image_cache() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("shot.png");
        // Minimal PNG signature — enough for MIME detection.
        std::fs::write(&path, [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']).unwrap();
        let history = vec![ChatMessage::user(format!(
            "look [IMAGE:{}]",
            path.display()
        ))];
        let cfg = MultimodalConfig::default();

        let mut cache = multimodal::LocalImageCache::new();
        let first = prepare_messages_for_iteration(&history, &cfg, false, Some(&mut cache))
            .await
            .unwrap();
        assert!(first.contains_images);
        assert_eq!(cache.len(), 1, "image cached after the first prep");

        // A later iteration/turn re-walks the same history; the cache serves it
        // without growing (no second disk read + encode).
        let _second = prepare_messages_for_iteration(&history, &cfg, false, Some(&mut cache))
            .await
            .unwrap();
        assert_eq!(cache.len(), 1, "subsequent preps reuse the cached entry");

        // The cache-less path (channels/CLI pass None) still resolves images.
        let uncached = prepare_messages_for_iteration(&history, &cfg, false, None)
            .await
            .unwrap();
        assert!(uncached.contains_images);
    }

    #[tokio::test]
    async fn prepare_strips_leading_assistant_tool_call() {
        let history = vec![
            ChatMessage::system("sys"),
            ChatMessage::assistant("[tool_call] fire"),
            ChatMessage::tool("result"),
            ChatMessage::user("actual user"),
        ];
        let cfg = MultimodalConfig::default();
        let prepared = prepare_messages_for_iteration(&history, &cfg, false, None)
            .await
            .unwrap();
        let first_non_system = prepared
            .messages
            .iter()
            .find(|m| m.role != "system")
            .expect("a non-system turn survives");
        assert_eq!(
            first_non_system.role, "user",
            "leading non-user turns must be dropped before dispatch"
        );
    }

    /// A tool-result `[AUDIO:...]` marker on the non-degrade path must be
    /// stripped before dispatch so a raw filesystem path never reaches the
    /// provider as literal text (the silent-hallucination failure mode). This
    /// exercises the real turn-loop prep entrypoint, not just the provider-layer
    /// helper, so it covers the degrade/non-degrade branch selection.
    #[tokio::test]
    async fn prepare_iteration_strips_tool_result_audio_marker() {
        let history = vec![
            ChatMessage::user("what do you hear in the clip?"),
            ChatMessage::tool("[AUDIO:/tmp/clip.wav] recorded 3:00 PM"),
        ];
        let cfg = MultimodalConfig::default();
        let prepared = prepare_messages_for_iteration(&history, &cfg, false, None)
            .await
            .unwrap();
        let joined = prepared
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !joined.contains("/tmp/clip.wav"),
            "audio path leaked to the provider payload: {joined}"
        );
        assert!(joined.contains("[media attachment]"));
    }

    #[tokio::test]
    async fn prepare_fails_closed_when_no_user_turn_survives() {
        let cfg = MultimodalConfig::default();

        // Pure system history.
        let system_only = vec![ChatMessage::system("sys")];
        let err = prepare_messages_for_iteration(&system_only, &cfg, false, None)
            .await
            .expect_err("system-only history must not reach the provider");
        assert!(
            err.to_string().contains("no user turn"),
            "expected a no-user-turn fail-closed error, got: {err}"
        );

        // Leading assistant/tool block with no anchoring user turn: sanitize
        // drains every non-system turn, leaving system-only, which must fail
        // closed rather than dispatch.
        let no_user = vec![
            ChatMessage::system("sys"),
            ChatMessage::assistant("[tool_call] fire"),
            ChatMessage::tool("result"),
        ];
        let err = prepare_messages_for_iteration(&no_user, &cfg, false, None)
            .await
            .expect_err("no-user history must not reach the provider");
        assert!(
            err.to_string().contains("no user turn"),
            "expected a no-user-turn fail-closed error, got: {err}"
        );
    }

    /// Regression: the dedicated vision route must resolve the configured
    /// `vision_model_provider`'s alias-specific `vision` override. The primary
    /// lacks vision and a `vision_model_provider` on a vision-capable family
    /// (llama.cpp) is forced `vision = false` on its alias. With config threaded,
    /// the route builds it through the alias-aware factory, so the forced-off
    /// provider is honored as non-vision and the capability error surfaces -
    /// proving the alias flag is read (the legacy `create_model_provider(vp,
    /// None)` path ignored it entirely).
    #[tokio::test]
    async fn resolve_vision_provider_honors_configured_alias_vision_override() {
        use zeroclaw_config::schema::{Config, MultimodalConfig};

        struct NonVisionPrimary;
        #[async_trait::async_trait]
        impl ModelProvider for NonVisionPrimary {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                Ok(String::new())
            }
        }
        impl zeroclaw_api::attribution::Attributable for NonVisionPrimary {
            fn role(&self) -> zeroclaw_api::attribution::Role {
                zeroclaw_api::attribution::Role::Provider(
                    zeroclaw_api::attribution::ProviderKind::Model(
                        zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "NonVisionPrimary"
            }
        }

        let config: Config = toml::from_str(
            r#"
schema_version = 3
[providers.models.llamacpp.forced_off]
model = "qwen3-4b"
vision = false
"#,
        )
        .expect("config parses");
        let multimodal = MultimodalConfig {
            vision_model_provider: Some("llamacpp.forced_off".to_string()),
            ..Default::default()
        };
        let history = vec![ChatMessage::user("look [IMAGE:/tmp/x.png]".to_string())];

        // `.err()` discards the Ok value (`Box<dyn ModelProvider>` is not `Debug`,
        // so `expect_err` will not compile).
        let err = resolve_vision_provider(
            Some(&config),
            &NonVisionPrimary,
            &history,
            &multimodal,
            "primary",
            "primary-model",
        )
        .await
        .err()
        .expect("a forced-off vision route must surface a capability error once its alias vision override is honored");
        assert!(
            err.to_string().contains("does not support vision"),
            "expected the vision-route capability error, got: {err}"
        );
    }

    /// Success-path companion to the error-branch test above: when the primary
    /// lacks vision and a configured `vision_model_provider` resolves to a
    /// vision-capable alias, the route builds it through the alias-aware factory
    /// and returns it for this iteration (no degrade).
    #[tokio::test]
    async fn resolve_vision_provider_builds_alias_and_dispatches_its_model() {
        use axum::{Json, Router, extract::State, routing::post};
        use serde_json::json;
        use tokio::sync::mpsc;
        use zeroclaw_config::schema::{Config, MultimodalConfig};

        async fn capture_model(
            State(tx): State<mpsc::UnboundedSender<String>>,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            tx.send(
                body.get("model")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
            .expect("test receiver remains open");
            Json(json!({
                "choices": [{"message": {"content": "ok"}}]
            }))
        }

        struct NonVisionPrimary;
        #[async_trait::async_trait]
        impl ModelProvider for NonVisionPrimary {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                Ok(String::new())
            }
        }
        impl zeroclaw_api::attribution::Attributable for NonVisionPrimary {
            fn role(&self) -> zeroclaw_api::attribution::Role {
                zeroclaw_api::attribution::Role::Provider(
                    zeroclaw_api::attribution::ProviderKind::Model(
                        zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "NonVisionPrimary"
            }
        }

        let (model_tx, mut model_rx) = mpsc::unbounded_channel();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test provider");
        let addr = listener.local_addr().expect("test provider address");
        let app = Router::new()
            .route("/v1/chat/completions", post(capture_model))
            .with_state(model_tx);
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app)
                .await
                .expect("test provider serves");
        });

        // A custom (OpenAI-compatible) alias defaults vision-capable. Its model
        // is canonical config and must travel with the provider through the
        // production dispatch boundary.
        let config: Config = toml::from_str(&format!(
            r#"
schema_version = 3
[providers.models.custom.myvision]
uri = "http://{addr}/v1"
model = "vision-model"
"#
        ))
        .expect("config parses");
        let multimodal = MultimodalConfig {
            vision_model_provider: Some("custom.myvision".to_string()),
            ..Default::default()
        };
        let history = vec![ChatMessage::user("look [IMAGE:/tmp/x.png]".to_string())];

        let (vision_provider, degrade) = resolve_vision_provider(
            Some(&config),
            &NonVisionPrimary,
            &history,
            &multimodal,
            "primary",
            "primary-model",
        )
        .await
        .expect("a configured vision-capable alias must build");
        let vision_provider =
            vision_provider.expect("the configured vision_model_provider must be returned");
        assert!(
            vision_provider
                .provider
                .capabilities_for_model(&vision_provider.model)
                .vision,
            "the resolved vision-route provider must support vision"
        );
        assert_eq!(vision_provider.model, "vision-model");
        assert!(
            !degrade,
            "a live vision route must not degrade/strip images"
        );
        let dispatch_messages = vec![ChatMessage::user("look")];
        zeroclaw_providers::ProviderDispatch::from_ref(vision_provider.provider.as_ref())
            .chat(
                zeroclaw_providers::ChatRequest {
                    messages: &dispatch_messages,
                    tools: None,
                    thinking: None,
                },
                &vision_provider.model,
                None,
            )
            .await
            .expect("resolved vision provider accepts the request");
        assert_eq!(
            model_rx.recv().await.expect("captured dispatched model"),
            "vision-model",
            "the alias-owned model must be the one sent to the selected endpoint"
        );

        let explicit = MultimodalConfig {
            vision_model_provider: Some("custom.myvision".to_string()),
            vision_model: Some("explicit-vision-model".to_string()),
            ..Default::default()
        };
        let (vision_provider, _) = resolve_vision_provider(
            Some(&config),
            &NonVisionPrimary,
            &history,
            &explicit,
            "primary",
            "primary-model",
        )
        .await
        .expect("an explicit vision model must resolve");
        assert_eq!(
            vision_provider
                .expect("the configured vision provider is returned")
                .model,
            "explicit-vision-model",
            "multimodal.vision_model must override the provider alias model"
        );
        server.abort();
    }

    /// Provider whose per-model vision capability is only known after its
    /// capability metadata is warmed — the "catalog-only image model" shape
    /// (e.g. a credentialed compatible provider that lists per-model vision
    /// through models.dev). Before warming, `capabilities_for_model` reports
    /// the family default (no vision); after warming, the model is vision.
    /// Records warm invocations.
    struct WarmThenVisionProvider {
        warm_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ModelProvider for WarmThenVisionProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }

        fn capabilities_for_model(
            &self,
            _model: &str,
        ) -> zeroclaw_api::model_provider::ProviderCapabilities {
            zeroclaw_api::model_provider::ProviderCapabilities {
                vision: self.warm_calls.load(std::sync::atomic::Ordering::SeqCst) >= 1,
                ..Default::default()
            }
        }

        async fn warm_capabilities_metadata(&self) {
            self.warm_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    impl zeroclaw_api::attribution::Attributable for WarmThenVisionProvider {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Provider(
                zeroclaw_api::attribution::ProviderKind::Model(
                    zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "WarmThenVisionProvider"
        }
    }

    /// Behavior invariant: a provider whose per-model vision is only known
    /// after its capability metadata is loaded (e.g. a credentialed compatible
    /// provider listing models through models.dev) must resolve as
    /// vision-capable under a warm-then-query order — images are not stripped
    /// and no dedicated vision route is needed. The capability resolve is
    /// itself atomic (warm + query in one owner), so both the direct turn and
    /// streamed entry points observe the warmed vision.
    #[tokio::test]
    async fn resolve_vision_provider_warms_primary_before_capability_decision() {
        let warm_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider = WarmThenVisionProvider {
            warm_calls: std::sync::Arc::clone(&warm_calls),
        };

        let multimodal = zeroclaw_config::schema::MultimodalConfig::default();
        let history = vec![ChatMessage::user("look [IMAGE:/tmp/x.png]".to_string())];

        let (vision_provider, degrade) = resolve_vision_provider(
            None,
            &provider,
            &history,
            &multimodal,
            "primary",
            "primary-model",
        )
        .await
        .expect(
            "warming must make the primary resolve as vision-capable; \
                 a provider whose capability only appears after warm must not error",
        );

        assert_eq!(
            warm_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "resolve_vision_provider must warm the PRIMARY provider before querying its capability"
        );
        assert!(
            vision_provider.is_none(),
            "a warmed vision-capable primary needs no secondary vision route"
        );
        assert!(
            !degrade,
            "images must NOT be stripped when the warmed primary supports vision"
        );
    }

    /// Behavior invariant (zero-overhead): a turn with NO image markers must
    /// never warm the provider's capability metadata. The catalog-backed warm
    /// is lazy to an actual image decision — an ordinary text turn does not
    /// wait on a models.dev request. If the warm were hoisted above the marker
    /// count check, this test fails with `warm_calls == 1`.
    #[tokio::test]
    async fn resolve_vision_provider_does_not_warm_on_text_only_history() {
        let warm_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider = WarmThenVisionProvider {
            warm_calls: std::sync::Arc::clone(&warm_calls),
        };

        let multimodal = zeroclaw_config::schema::MultimodalConfig::default();
        let history = vec![ChatMessage::user("just text, no media".to_string())];

        let (vision_provider, degrade) = resolve_vision_provider(
            None,
            &provider,
            &history,
            &multimodal,
            "primary",
            "primary-model",
        )
        .await
        .expect("a text-only turn must resolve without any warm or capability query");

        assert_eq!(
            warm_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a text-only turn must not invoke the catalog-backed capability warm"
        );
        assert!(
            vision_provider.is_none(),
            "no image markers means no vision route is needed"
        );
        assert!(!degrade, "no image markers means nothing is stripped");
    }

    /// Provider whose capability resolve fails — the "catalog unknown" shape
    /// (a models.dev outage or retry-backoff). Its capability query reports no
    /// vision, but callers must NOT read that as an authoritative negative: the
    /// `resolve_capabilities_for_model` `Err` is the signal that capability is
    /// unknown, distinct from a real no-vision answer.
    struct ResolveErrPrimary;

    #[async_trait::async_trait]
    impl ModelProvider for ResolveErrPrimary {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }

        async fn resolve_capabilities_for_model(
            &self,
            _model: &str,
        ) -> anyhow::Result<zeroclaw_api::model_provider::ProviderCapabilities> {
            Err(anyhow::Error::msg("test catalog outage"))
        }
    }
    impl zeroclaw_api::attribution::Attributable for ResolveErrPrimary {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Provider(
                zeroclaw_api::attribution::ProviderKind::Model(
                    zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "ResolveErrPrimary"
        }
    }

    /// 08-13 regression: when the primary provider's capability resolve fails
    /// (catalog unknown) but a dedicated `vision_model_provider` is configured,
    /// the unknown must NOT be read as "no vision" that errors out — the
    /// configured vision route stays usable and is returned for the iteration.
    #[tokio::test]
    async fn resolve_vision_provider_routes_to_configured_vision_on_primary_unknown() {
        use axum::{Json, Router, extract::State, routing::post};
        use serde_json::json;
        use tokio::sync::mpsc;
        use zeroclaw_config::schema::{Config, MultimodalConfig};

        async fn ok_json(
            State(_tx): State<mpsc::UnboundedSender<String>>,
        ) -> Json<serde_json::Value> {
            Json(json!({
                "choices": [{"message": {"content": "ok"}}]
            }))
        }

        let (tx, _rx) = mpsc::unbounded_channel();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test provider");
        let addr = listener.local_addr().expect("test provider address");
        let app = Router::new()
            .route("/v1/chat/completions", post(ok_json))
            .with_state(tx);
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app)
                .await
                .expect("test provider serves");
        });

        let config: Config = toml::from_str(&format!(
            r#"
schema_version = 3
[providers.models.custom.myvision]
uri = "http://{addr}/v1"
model = "vision-model"
"#
        ))
        .expect("config parses");
        let multimodal = MultimodalConfig {
            vision_model_provider: Some("custom.myvision".to_string()),
            ..Default::default()
        };
        let history = vec![ChatMessage::user("look [IMAGE:/tmp/x.png]".to_string())];

        // The primary capability is UNKNOWN (catalog outage). A configured vision
        // provider must still be used, not skipped behind a spurious "no vision".
        let (vision_provider, degrade) = resolve_vision_provider(
            Some(&config),
            &ResolveErrPrimary,
            &history,
            &multimodal,
            "primary",
            "primary-model",
        )
        .await
        .expect("a configured vision route must survive a primary capability unknown");
        let vision_provider =
            vision_provider.expect("the configured vision_model_provider must be returned");
        assert_eq!(vision_provider.model, "vision-model");
        assert_eq!(vision_provider.provider_name, "custom.myvision");
        assert!(
            !degrade,
            "routing to a configured vision provider must not degrade/strip images"
        );
        server.abort();
    }

    /// 08-13 regression: primary capability unknown + a latest-user image + no
    /// configured vision provider must surface the failure rather than reporting
    /// "does not support vision" on the primary (which would be a false negative).
    #[tokio::test]
    async fn resolve_vision_provider_surfaces_primary_unknown_on_latest_user_image() {
        let multimodal = zeroclaw_config::schema::MultimodalConfig::default();
        let history = vec![ChatMessage::user("look [IMAGE:/tmp/x.png]".to_string())];

        let err = resolve_vision_provider(
            None,
            &ResolveErrPrimary,
            &history,
            &multimodal,
            "primary",
            "primary-model",
        )
        .await
        .err()
        .expect("a latest-user image with unknown capability and no vision route must surface the failure");
        let text = err.to_string();
        assert!(
            text.contains("could not be resolved") || text.contains("catalog"),
            "the surfaced error must describe the capability lookup failure, got: {text}"
        );
        assert!(
            !text.contains("does not support vision input"),
            "an unknown must not be reported as authoritative no-vision, got: {text}"
        );
    }

    /// 08-13 regression: a carried-over (earlier-user) image with primary
    /// capability unknown and no vision route must surface the failure — NOT
    /// strip the image to a text-only degrade (unknown must be distinguished
    /// from the authoritative no-vision that legitimately degrades).
    #[tokio::test]
    async fn resolve_vision_provider_does_not_strip_on_primary_unknown_carried_over() {
        let multimodal = zeroclaw_config::schema::MultimodalConfig::default();
        // Earlier user message carries the image; the NEWEST user message does
        // not, so this is a carried-over image (no authoritative no-vision result
        // to legitimately degrade).
        let history = vec![
            ChatMessage::user("look [IMAGE:/tmp/x.png]".to_string()),
            ChatMessage::user("continue".to_string()),
        ];

        let err = resolve_vision_provider(
            None,
            &ResolveErrPrimary,
            &history,
            &multimodal,
            "primary",
            "primary-model",
        )
        .await
        .err()
        .expect("a carried-over image with unknown capability must surface, not silently strip");

        // Degrade-strip is a real output, so the returned error proves the
        // resolver did NOT take the "confirmed no-vision, strip" path.
        let text = err.to_string();
        assert!(
            text.contains("could not be resolved") || text.contains("catalog"),
            "the surfaced error must describe the capability lookup failure, got: {text}"
        );
    }
}
