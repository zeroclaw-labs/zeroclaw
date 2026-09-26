//! Vision model-provider routing and per-iteration message preparation.

use std::sync::Arc;

use anyhow::Result;
use zeroclaw_config::schema::{Config, MultimodalConfig};
use zeroclaw_providers::{ChatMessage, ModelProvider, ProviderCapabilityError, multimodal};

use crate::security::SecurityPolicy;

pub(crate) struct ResolvedVisionProvider {
    pub(crate) provider: Box<dyn ModelProvider>,
    pub(crate) provider_name: String,
    pub(crate) model: String,
}

/// The exact `file_read` read ledger for one image-marker path: the
/// string-level `is_path_allowed` check first (no filesystem access for a
/// path it rejects), then the same symlink-aware readability check
/// `file_read` applies (`resolve_tool_path` + `is_resolved_path_readable`).
/// This does synchronous filesystem work (symlink resolution, metadata),
/// so it must only be called from the blocking task inside
/// `multimodal::count_latest_user_resolvable_image_markers`, never on the
/// async executor thread.
pub(crate) fn policy_permits_image_path(policy: &SecurityPolicy, path: &std::path::Path) -> bool {
    let path_str = path.to_string_lossy();
    policy.is_path_allowed(&path_str)
        && policy.is_resolved_path_readable(&policy.resolve_tool_path(&path_str))
}

/// Decide this turn's image-input route.
///
/// No image markers in the history, or a primary provider that already
/// accepts images (`capabilities_for_model(dispatch_model)`): the primary
/// serves the turn unchanged, `(None, false)`. A non-vision primary with a
/// configured `[multimodal] vision_model_provider`: the configured provider
/// is built through the alias-aware factory and returned, `(Some(_), false)`;
/// a configured provider that itself lacks vision is an operator
/// misconfiguration and surfaces as a `ProviderCapabilityError`.
///
/// `security` governs one thing: which local image-marker references in the
/// latest user message count as "resolvable" for the no-vision, no-fallback
/// refusal. A local reference counts only when it is absolute, the policy's
/// string-level check admits it, the same symlink-aware read ledger the
/// `file_read` tool applies admits the resolved target, and the file exists.
/// The resolvable count is computed only on that no-vision, no-fallback
/// branch: a vision-capable primary or a configured vision fallback never
/// touches the filesystem for this decision, and the work is bounded by the
/// absolute local-path markers of the latest user message, one message,
/// never the whole history: at most `multimodal::MAX_RESOLVABILITY_CANDIDATES`
/// (16) of them reach the policy check plus one `std::fs::metadata` probe,
/// all on a single `spawn_blocking` task off the async executor; markers past
/// that bound count as resolvable before any policy call. A path the policy
/// rejects, a relative reference, a data URI, or a remote URL is never
/// probed. `None` fails closed: no local path resolves, so a configless
/// caller degrades to a text-only turn instead of erroring, and data-URI
/// and remote references are unaffected by `None`.
pub(crate) async fn resolve_vision_provider(
    config: Option<&Config>,
    model_provider: &dyn ModelProvider,
    history: &[ChatMessage],
    multimodal_config: &MultimodalConfig,
    provider_name: &str,
    model: &str,
    dispatch_model: &str,
    security: Option<&SecurityPolicy>,
) -> Result<(Option<ResolvedVisionProvider>, bool)> {
    let image_marker_count = multimodal::count_image_markers(history);
    let latest_user_image_marker_count = multimodal::count_latest_user_image_markers(history);

    let mut degrade_strip_images = false;
    let vision_model_provider: Option<ResolvedVisionProvider> = if image_marker_count > 0
        && !model_provider.capabilities_for_model(dispatch_model).vision
    {
        if let Some(ref vp) = multimodal_config.vision_model_provider {
            // Resolve the configured vision provider through the alias-aware
            // factory so its per-alias `vision` override and typed config
            // (endpoint URI, credentials) are honored - the legacy
            // `create_model_provider(vp, None)` passed `config = None` and could
            // not see them, so a text-family alias forced to `vision = true`
            // for this route would have been ignored. `config` is `None` only on
            // configless (test-builder) agents - every production agent/loop path
            // threads `Some`; that fallback keeps the prior legacy behavior.
            let (vp_instance, alias_model) = match config {
                Some(config) => {
                    zeroclaw_providers::create_model_provider_from_ref_with_model(config, vp)
                        .map(|resolved| (resolved.provider, resolved.model))
                }
                None => zeroclaw_providers::create_model_provider(vp, None)
                    .map(|provider| (provider, None)),
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
            if !vp_instance.capabilities_for_model(&vision_model).vision {
                // Operator misconfiguration (named a non-vision provider as
                // the vision route) — surface it loudly rather than silently
                // degrading.
                return Err(ProviderCapabilityError {
                    model_provider: vp.clone(),
                    capability: "vision".to_string(),
                    message: format!(
                        "configured vision_model_provider '{vp}' does not support vision input"
                    ),
                }
                .into());
            }
            Some(ResolvedVisionProvider {
                provider: vp_instance,
                provider_name: vp.clone(),
                model: vision_model,
            })
        } else {
            // A local marker reference counts as resolvable only when the
            // agent's filesystem policy would let its file tools read it:
            // the string-level `is_path_allowed` check runs first (no
            // filesystem access at all for a path it rejects), and a
            // surviving reference is then held to the same symlink-aware
            // read ledger `file_read` applies (`resolve_tool_path` +
            // `is_resolved_path_readable`). That ledger does synchronous
            // filesystem work (symlink resolution, metadata), so it runs
            // on ONE `spawn_blocking` task inside
            // `count_latest_user_resolvable_image_markers`, never on the
            // async executor thread.
            //
            // This is the ONLY place the resolvable count is computed: a
            // vision-capable primary or a configured vision fallback never
            // touches the filesystem for this decision. The work is
            // bounded by `multimodal::MAX_RESOLVABILITY_CANDIDATES` (16)
            // absolute local-path markers of the latest user message (one
            // message, never the whole history); markers past the bound
            // are counted as resolvable BEFORE any policy call, so an
            // oversized message fails toward this error rather than a
            // silent degrade, and cannot buy unbounded policy work with
            // unbounded markers. Without a policy (`None`) no local path
            // resolves: fail closed to the degrade branch.
            //
            // The policy is cloned once per gate evaluation onto the
            // blocking task (plain data: Vecs, PathBufs, Strings, and a
            // cheaply-cloned tracker), and this branch is the cold
            // no-vision, no-fallback path, so the clone never runs on a
            // hot vision-capable turn.
            let policy = security.cloned();
            let path_allowed: Arc<dyn Fn(&std::path::Path) -> bool + Send + Sync> =
                Arc::new(move |path: &std::path::Path| {
                    policy
                        .as_ref()
                        .is_some_and(|policy| policy_permits_image_path(policy, path))
                });
            let latest_user_resolvable_marker_count =
                multimodal::count_latest_user_resolvable_image_markers(
                    history,
                    multimodal_config.allow_remote_fetch,
                    path_allowed,
                )
                .await;
            if latest_user_resolvable_marker_count > 0 {
                // Marker syntax alone must not fail the turn: prose that
                // discusses marker syntax parses into markers whose references
                // resolve to nothing (a missing file, a malformed data URI, a
                // remote URL while remote fetch is off). Only references that
                // would actually be sent reach this hard error, so the count in
                // the refusal is the count of loadable attachments; the rest
                // fall through to the degrade branch and the turn proceeds as
                // text.
                //
                // `vision_limited_by` already excludes the primary entry (it
                // returns `None` when the primary itself is the non-vision
                // entry), so any `Some` here names a genuine fallback and is
                // safe to surface without re-deriving primary-vs-fallback from
                // `provider_name`, whose format is not guaranteed to line up
                // with the dotted entry name.
                let marker_count = latest_user_resolvable_marker_count.to_string();
                let message = match model_provider.vision_limited_by(model) {
                    Some(fallback_name) => crate::i18n::get_required_cli_string_with_args(
                        "cli-agent-vision-unsupported-by-fallback",
                        &[
                            ("marker_count", marker_count.as_str()),
                            ("fallback_name", fallback_name.as_str()),
                        ],
                    ),
                    None => crate::i18n::get_required_cli_string_with_args(
                        "cli-agent-vision-unsupported-by-provider",
                        &[("marker_count", marker_count.as_str())],
                    ),
                };
                return Err(ProviderCapabilityError {
                    model_provider: provider_name.to_string(),
                    capability: "vision".to_string(),
                    message,
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
                            "latest_user_image_marker_count": latest_user_image_marker_count,
                            "latest_user_resolvable_marker_count":
                                latest_user_resolvable_marker_count,
                        })),
                    "no vision route for image marker(s) that are carried over, tool results, or unresolvable; degrading to text-only (markers stripped)"
                );
                degrade_strip_images = true;
                None
            }
        }
    } else {
        None
    };

    Ok((vision_model_provider, degrade_strip_images))
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
        // Text-only fallback: replace every media marker with the prose
        // placeholder so no filesystem path or data URI reaches the
        // text-only provider, while surrounding text (captions, tool
        // metadata) survives. An assistant tool-call envelope is rewritten
        // field-wise instead: signed thinking (`reasoning_content`) and
        // tool-call signatures (`tool_calls[].extra_content`) must replay
        // byte-for-byte, and a composite provider (the reliable wrapper)
        // reports no vision whenever one of its fallbacks lacks it — so the
        // primary that receives this degraded request may be the very
        // provider that verifies those signatures.
        let stripped: Vec<ChatMessage> = history
            .iter()
            .map(|m| ChatMessage {
                role: m.role.clone(),
                content: multimodal::strip_media_markers_model_visible(m),
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
        assert!(joined.contains(multimodal::MEDIA_PLACEHOLDER));
    }

    /// The text-only degrade path used to map `strip_media_markers` over
    /// every message's whole string, rewriting a marker inside an assistant
    /// envelope's signed reasoning while keeping its signature. The rewrite
    /// is field-wise for envelopes now, so the reasoning replays
    /// byte-for-byte even though the provider is text-only. The envelope is
    /// built with the production `build_native_assistant_history` builder
    /// the adapters parse back; the `/tmp` paths are literal text only —
    /// nothing is read from disk on the degrade path.
    #[tokio::test]
    async fn degrade_strips_markers_field_wise_in_assistant_envelope() {
        let path = "/tmp/a.png";
        let marker = format!("[{}:{}]", "IMAGE", path);
        let reasoning =
            format!(r#"{{"thinking":"check {marker} before answering","signature":"sig_abc"}}"#);
        let envelope = super::super::parse_response::build_native_assistant_history(
            &format!("saved {marker}"),
            &[zeroclaw_api::model_provider::ToolCall {
                id: "toolu_1".into(),
                name: "shell".into(),
                arguments: "{}".into(),
                extra_content: Some(serde_json::json!({
                    "google": {"thought_signature": "sig_gemini"}
                })),
            }],
            Some(&reasoning),
        );
        let history = vec![
            ChatMessage::user(format!("look {marker}")),
            ChatMessage::assistant(envelope),
            ChatMessage::tool("done"),
        ];
        let cfg = MultimodalConfig::default();
        let prepared = prepare_messages_for_iteration(&history, &cfg, true, None)
            .await
            .unwrap();

        let assistant_prepared = prepared
            .messages
            .iter()
            .find(|m| m.role == "assistant")
            .expect("assistant envelope survives the degrade path");
        let parsed: serde_json::Value =
            serde_json::from_str(&assistant_prepared.content).expect("envelope stays valid JSON");
        assert_eq!(
            parsed["reasoning_content"].as_str(),
            Some(reasoning.as_str()),
            "signed thinking must survive the degrade path byte-for-byte"
        );
        assert_eq!(
            parsed["tool_calls"],
            serde_json::json!([{
                "id": "toolu_1",
                "name": "shell",
                "arguments": "{}",
                "extra_content": {"google": {"thought_signature": "sig_gemini"}},
            }]),
            "tool calls (including extra_content signatures) must round-trip unchanged"
        );
        let content = parsed["content"].as_str().expect("content stays a string");
        assert!(
            content.contains(multimodal::MEDIA_PLACEHOLDER),
            "the envelope's content marker is replaced: {content}"
        );
        assert!(
            !content.contains(path),
            "no raw path may survive in the envelope's content: {content}"
        );

        let user_prepared = prepared
            .messages
            .iter()
            .find(|m| m.role == "user")
            .expect("user message survives the degrade path");
        assert!(
            user_prepared
                .content
                .contains(multimodal::MEDIA_PLACEHOLDER),
            "the whole-string rule still applies to other roles: {}",
            user_prepared.content
        );
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
            "primary-model",
            None,
        )
        .await
        .err()
        .expect("a forced-off vision route must surface a capability error once its alias vision override is honored");
        assert!(
            err.to_string().contains("does not support vision"),
            "expected the vision-route capability error, got: {err}"
        );
    }

    /// Regression: when the primary is an aggregate (e.g. a reliable
    /// model_provider) whose non-vision entry is a configured fallback, the
    /// capability error must name that fallback rather than blaming the
    /// primary, since the primary itself may well support vision.
    #[tokio::test]
    async fn resolve_vision_provider_names_fallback_in_capability_error() {
        struct NonVisionWithNamedFallback;
        #[async_trait::async_trait]
        impl ModelProvider for NonVisionWithNamedFallback {
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
                    vision: false,
                    ..Default::default()
                }
            }
            fn vision_limited_by(&self, _model: &str) -> Option<String> {
                Some("zai.default".to_string())
            }
        }
        impl zeroclaw_api::attribution::Attributable for NonVisionWithNamedFallback {
            fn role(&self) -> zeroclaw_api::attribution::Role {
                zeroclaw_api::attribution::Role::Provider(
                    zeroclaw_api::attribution::ProviderKind::Model(
                        zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "NonVisionWithNamedFallback"
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("shot.png");
        std::fs::write(&image_path, b"existence is all the gate checks").unwrap();
        let security = SecurityPolicy {
            workspace_dir: temp.path().to_path_buf(),
            workspace_only: true,
            ..SecurityPolicy::default()
        };
        let multimodal = MultimodalConfig::default();
        let history = vec![ChatMessage::user(format!(
            "look [IMAGE:{}]",
            image_path.display()
        ))];

        let err = resolve_vision_provider(
            None,
            &NonVisionWithNamedFallback,
            &history,
            &multimodal,
            "primary",
            "primary-model",
            "primary-model",
            Some(&security),
        )
        .await
        .err()
        .expect("a non-vision aggregate with no vision route must surface a capability error");

        let capability_error = err
            .downcast_ref::<ProviderCapabilityError>()
            .expect("vision refusal must retain its structured capability error");
        assert_eq!(capability_error.model_provider, "primary");
        assert_eq!(capability_error.capability, "vision");
        assert!(
            capability_error.message.contains("zai.default"),
            "the localized refusal must name the limiting fallback: {capability_error}"
        );
    }

    /// Companion to the fallback-naming test above: a lone non-vision
    /// provider (no aggregate, so nothing names a fallback) must keep the
    /// original wording rather than being mislabeled as a fallback problem.
    #[tokio::test]
    async fn resolve_vision_provider_keeps_primary_wording_without_a_named_fallback() {
        struct PlainNonVisionPrimary;
        #[async_trait::async_trait]
        impl ModelProvider for PlainNonVisionPrimary {
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
        impl zeroclaw_api::attribution::Attributable for PlainNonVisionPrimary {
            fn role(&self) -> zeroclaw_api::attribution::Role {
                zeroclaw_api::attribution::Role::Provider(
                    zeroclaw_api::attribution::ProviderKind::Model(
                        zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "PlainNonVisionPrimary"
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("shot.png");
        std::fs::write(&image_path, b"existence is all the gate checks").unwrap();
        let security = SecurityPolicy {
            workspace_dir: temp.path().to_path_buf(),
            workspace_only: true,
            ..SecurityPolicy::default()
        };
        let multimodal = MultimodalConfig::default();
        let history = vec![ChatMessage::user(format!(
            "look [IMAGE:{}]",
            image_path.display()
        ))];

        let err = resolve_vision_provider(
            None,
            &PlainNonVisionPrimary,
            &history,
            &multimodal,
            "primary",
            "primary-model",
            "primary-model",
            Some(&security),
        )
        .await
        .err()
        .expect("a non-vision primary with no vision route must surface a capability error");

        let capability_error = err
            .downcast_ref::<ProviderCapabilityError>()
            .expect("vision refusal must retain its structured capability error");
        assert_eq!(capability_error.model_provider, "primary");
        assert_eq!(capability_error.capability, "vision");
        assert!(
            !capability_error.message.is_empty(),
            "the localized primary-provider refusal must remain user-visible"
        );
    }

    /// Marker-shaped prose whose references resolve to nothing (a missing
    /// file, a malformed data URI) must not fail the turn on a non-vision
    /// provider: it takes the same degrade branch as carried-over markers,
    /// so the turn proceeds with the markers stripped.
    #[tokio::test]
    async fn no_vision_provider_with_unresolvable_marker_degrades_instead_of_failing() {
        struct PlainNonVisionPrimary;
        #[async_trait::async_trait]
        impl ModelProvider for PlainNonVisionPrimary {
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
        impl zeroclaw_api::attribution::Attributable for PlainNonVisionPrimary {
            fn role(&self) -> zeroclaw_api::attribution::Role {
                zeroclaw_api::attribution::Role::Provider(
                    zeroclaw_api::attribution::ProviderKind::Model(
                        zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "PlainNonVisionPrimary"
            }
        }

        let multimodal = MultimodalConfig::default();
        let missing = "/definitely/not/a/real/shot.png";
        let malformed_uri = "data:image/png;base64,%%%";
        let history = vec![ChatMessage::user(format!(
            "the brief says [IMAGE:{missing}] and [IMAGE:{malformed_uri}] as fixtures"
        ))];

        let (vision_provider, degrade_strip_images) = resolve_vision_provider(
            None,
            &PlainNonVisionPrimary,
            &history,
            &multimodal,
            "primary",
            "primary-model",
            "primary-model",
            None,
        )
        .await
        .expect("unresolvable markers must degrade instead of failing the turn");
        assert!(
            vision_provider.is_none(),
            "no vision route exists, so the degrade branch must return None"
        );
        assert!(
            degrade_strip_images,
            "marker-shaped prose must be stripped so the turn proceeds as text"
        );
    }

    /// A marker whose reference resolves (an existing file) still fails the
    /// turn on a non-vision provider, and the refusal counts the loadable
    /// marker(s) rather than every marker-shaped span in the text.
    #[tokio::test]
    async fn no_vision_provider_with_resolvable_marker_still_fails() {
        struct PlainNonVisionPrimary;
        #[async_trait::async_trait]
        impl ModelProvider for PlainNonVisionPrimary {
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
        impl zeroclaw_api::attribution::Attributable for PlainNonVisionPrimary {
            fn role(&self) -> zeroclaw_api::attribution::Role {
                zeroclaw_api::attribution::Role::Provider(
                    zeroclaw_api::attribution::ProviderKind::Model(
                        zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "PlainNonVisionPrimary"
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("shot.png");
        std::fs::write(&image_path, b"existence is all the gate checks").unwrap();
        let security = SecurityPolicy {
            workspace_dir: temp.path().to_path_buf(),
            workspace_only: true,
            ..SecurityPolicy::default()
        };
        let multimodal = MultimodalConfig::default();
        let history = vec![ChatMessage::user(format!(
            "look at this [IMAGE:{}]",
            image_path.display()
        ))];

        let err = resolve_vision_provider(
            None,
            &PlainNonVisionPrimary,
            &history,
            &multimodal,
            "primary",
            "primary-model",
            "primary-model",
            Some(&security),
        )
        .await
        .err()
        .expect("a resolvable image marker on a non-vision provider must fail");

        let capability_error = err
            .downcast_ref::<ProviderCapabilityError>()
            .expect("vision refusal must retain its structured capability error");
        assert_eq!(capability_error.model_provider, "primary");
        assert_eq!(capability_error.capability, "vision");
        assert!(
            capability_error.message.contains("1 image marker(s)"),
            "the refusal must count the loadable marker: {capability_error}"
        );
    }

    /// The production predicate, bounded and off the executor: a real
    /// `SecurityPolicy` (workspace_dir + workspace_only) behind
    /// `policy_permits_image_path`, 20 absolute markers in one user
    /// message. Only the first 16 (the candidate bound) reach the
    /// predicate at all, and they reach it on the blocking thread: the
    /// first call runs the same entered/release handshake as the
    /// providers-side off-executor test, which can only pass while the
    /// runtime's single thread stays responsive, i.e. while the
    /// predicate is NOT running on it. This is the budget regression
    /// the reviewers asked for: after the bound is spent, overflow
    /// markers count WITHOUT any policy call, so the unbounded
    /// symlink-resolving policy work of the old shape is gone. The
    /// dangling symlink at position 3 exercises the `read_link` branch
    /// of `resolve_symlinked_path` (it resolves to an in-workspace
    /// missing target, so the policy allows it and only the existence
    /// probe rejects it). Do NOT "simplify" this onto a multi_thread
    /// runtime; see the providers-side test for why.
    #[tokio::test]
    async fn production_policy_predicate_is_bounded_and_runs_off_the_executor() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        let first = workspace.path().join("first.png");
        let second = workspace.path().join("second.png");
        std::fs::write(&first, png).unwrap();
        std::fs::write(&second, png).unwrap();
        let dangling = workspace.path().join("dangling.png");
        let missing_target = workspace.path().join("no-such-target.png");
        // Position 3: a dangling symlink inside the workspace. On
        // non-unix there is no symlink to make, so the path simply stays
        // missing and the expected counts are unchanged.
        #[cfg(unix)]
        std::os::unix::fs::symlink(&missing_target, &dangling).unwrap();
        let outside_file = outside.path().join("host-secret.png");
        std::fs::write(&outside_file, b"an existing file outside the read boundary").unwrap();
        let policy = SecurityPolicy {
            workspace_dir: workspace.path().to_path_buf(),
            workspace_only: true,
            ..SecurityPolicy::default()
        };

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        // Neither mpsc end is `Sync`, and the predicate must be; the
        // mutexes make the captured sender and receiver shareable without
        // changing the handshake (the predicate blocks in `recv_timeout`
        // inside its mutex while the watcher holds no lock).
        let entered_tx = std::sync::Mutex::new(entered_tx);
        let release_rx = std::sync::Mutex::new(release_rx);
        let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let calls_for_predicate = std::sync::Arc::clone(&calls);
        let released_for_predicate = std::sync::Arc::clone(&released);
        let path_allowed: Arc<dyn Fn(&std::path::Path) -> bool + Send + Sync> =
            Arc::new(move |path: &std::path::Path| -> bool {
                let first_call =
                    calls_for_predicate.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
                if first_call {
                    entered_tx
                        .lock()
                        .expect("entered sender lockable")
                        .send(())
                        .expect("entered signal sendable");
                    let arrived = release_rx
                        .lock()
                        .expect("release receiver lockable")
                        .recv_timeout(std::time::Duration::from_secs(5));
                    released_for_predicate
                        .store(arrived.is_ok(), std::sync::atomic::Ordering::SeqCst);
                }
                policy_permits_image_path(&policy, path)
            });
        // The watcher runs detached on the runtime's own thread pool via
        // the sanctioned spawn wrapper (plain `tokio::spawn` is
        // workspace-disallowed); its handle is intentionally dropped.
        let _watcher = zeroclaw_spawn::spawn!(async move {
            loop {
                if entered_rx.try_recv().is_ok() {
                    release_tx.send(()).expect("release signal sendable");
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });

        let mut marker_paths = vec![first, second, dangling, outside_file];
        for i in 5..=20 {
            marker_paths.push(workspace.path().join(format!("missing-{i}.png")));
        }
        let marker_text = marker_paths
            .iter()
            .map(|path| format!("[IMAGE: {}]", path.display()))
            .collect::<Vec<_>>()
            .join(" ");
        let messages = vec![ChatMessage::user(format!("look at these {marker_text}"))];
        assert_eq!(
            multimodal::count_latest_user_resolvable_image_markers(&messages, false, path_allowed)
                .await,
            2 + (20 - multimodal::MAX_RESOLVABILITY_CANDIDATES),
            "two existing in-workspace files count; the dangling symlink, the \
             outside file, and the missing files do not; the four overflow \
             markers count without any check"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            multimodal::MAX_RESOLVABILITY_CANDIDATES,
            "the production predicate runs only for the 16 candidates; the 4 \
             overflow markers get no policy call at all"
        );
        assert!(
            released.load(std::sync::atomic::Ordering::SeqCst),
            "the runtime-thread watcher observed the production predicate and \
             released it, so the policy work ran off the executor thread"
        );
    }

    /// The overflow count reaches the refusal through the production
    /// wiring: 20 absolute markers with two existing in-workspace files
    /// among the first 16 candidates, through the real
    /// `resolve_vision_provider` on the no-vision, no-fallback path.
    /// The refusal must count 6: the two loadable candidates plus the
    /// four overflow markers that were never checked. Pins that the
    /// candidate-bound semantics (overflow counts as resolvable) is
    /// what the turn actually sees, not just what the helper returns.
    #[tokio::test]
    async fn resolve_vision_provider_refuses_with_overflow_count_on_non_vision_provider() {
        struct PlainNonVisionPrimary;
        #[async_trait::async_trait]
        impl ModelProvider for PlainNonVisionPrimary {
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
        impl zeroclaw_api::attribution::Attributable for PlainNonVisionPrimary {
            fn role(&self) -> zeroclaw_api::attribution::Role {
                zeroclaw_api::attribution::Role::Provider(
                    zeroclaw_api::attribution::ProviderKind::Model(
                        zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "PlainNonVisionPrimary"
            }
        }

        let workspace = tempfile::tempdir().unwrap();
        let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        let first = workspace.path().join("first.png");
        let second = workspace.path().join("second.png");
        std::fs::write(&first, png).unwrap();
        std::fs::write(&second, png).unwrap();
        let security = SecurityPolicy {
            workspace_dir: workspace.path().to_path_buf(),
            workspace_only: true,
            ..SecurityPolicy::default()
        };
        let multimodal = MultimodalConfig::default();
        let mut marker_paths = vec![first, second];
        for i in 3..=20 {
            marker_paths.push(workspace.path().join(format!("missing-{i}.png")));
        }
        let marker_text = marker_paths
            .iter()
            .map(|path| format!("[IMAGE: {}]", path.display()))
            .collect::<Vec<_>>()
            .join(" ");
        let history = vec![ChatMessage::user(format!("look at these {marker_text}"))];

        let err = resolve_vision_provider(
            None,
            &PlainNonVisionPrimary,
            &history,
            &multimodal,
            "primary",
            "primary-model",
            "primary-model",
            Some(&security),
        )
        .await
        .err()
        .expect("resolvable markers on a non-vision provider must fail the turn");

        let capability_error = err
            .downcast_ref::<ProviderCapabilityError>()
            .expect("vision refusal must retain its structured capability error");
        assert_eq!(capability_error.model_provider, "primary");
        assert_eq!(capability_error.capability, "vision");
        assert!(
            capability_error.message.contains("6 image marker(s)"),
            "the refusal must count the two loadable candidates plus the four \
             unchecked overflow markers: {capability_error}"
        );
    }

    /// The existence-oracle regression the policy gate exists for: a marker
    /// whose file exists OUTSIDE the agent's read boundary must take the
    /// exact same branch as when no file exists anywhere, so a message author
    /// cannot learn host-file existence from the error-vs-degrade outcome on
    /// a model that will never read the file.
    #[tokio::test]
    async fn unresolvable_outside_workspace_marker_degrades_without_probe() {
        struct PlainNonVisionPrimary;
        #[async_trait::async_trait]
        impl ModelProvider for PlainNonVisionPrimary {
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
        impl zeroclaw_api::attribution::Attributable for PlainNonVisionPrimary {
            fn role(&self) -> zeroclaw_api::attribution::Role {
                zeroclaw_api::attribution::Role::Provider(
                    zeroclaw_api::attribution::ProviderKind::Model(
                        zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "PlainNonVisionPrimary"
            }
        }
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_path = outside.path().join("host-secret.png");
        std::fs::write(&outside_path, b"an existing file outside the read boundary").unwrap();
        let inside_missing = workspace.path().join("attachment.png");
        let security = SecurityPolicy {
            workspace_dir: workspace.path().to_path_buf(),
            workspace_only: true,
            ..SecurityPolicy::default()
        };
        let multimodal = MultimodalConfig::default();
        let history = vec![ChatMessage::user(format!(
            "see [IMAGE: {}] and [IMAGE: {}]",
            outside_path.display(),
            inside_missing.display()
        ))];

        let (vision_provider, degrade_strip_images) = resolve_vision_provider(
            None,
            &PlainNonVisionPrimary,
            &history,
            &multimodal,
            "primary",
            "primary-model",
            "primary-model",
            Some(&security),
        )
        .await
        .expect("markers outside the read boundary must degrade, never fail the turn");
        assert!(
            vision_provider.is_none(),
            "no vision route exists, so the degrade branch must return None"
        );
        assert!(
            degrade_strip_images,
            "an unreadable marker must be stripped so the turn proceeds as text"
        );

        // The same message with both files missing takes the identical
        // branch. Before the policy gate, the existing outside file flipped
        // the first call to the capability error: that flip is the oracle.
        std::fs::remove_file(&outside_path).unwrap();
        let (vision_provider, degrade_strip_images) = resolve_vision_provider(
            None,
            &PlainNonVisionPrimary,
            &history,
            &multimodal,
            "primary",
            "primary-model",
            "primary-model",
            Some(&security),
        )
        .await
        .expect("missing files must degrade, never fail the turn");
        assert!(vision_provider.is_none());
        assert!(degrade_strip_images);
    }

    /// A marker whose file the policy DOES allow to be read (inside the
    /// workspace) still refuses on a non-vision provider with no fallback.
    #[tokio::test]
    async fn inside_workspace_marker_still_refuses() {
        struct PlainNonVisionPrimary;
        #[async_trait::async_trait]
        impl ModelProvider for PlainNonVisionPrimary {
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
        impl zeroclaw_api::attribution::Attributable for PlainNonVisionPrimary {
            fn role(&self) -> zeroclaw_api::attribution::Role {
                zeroclaw_api::attribution::Role::Provider(
                    zeroclaw_api::attribution::ProviderKind::Model(
                        zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "PlainNonVisionPrimary"
            }
        }
        let workspace = tempfile::tempdir().unwrap();
        let image_path = workspace.path().join("shot.png");
        std::fs::write(&image_path, b"existence is all the gate checks").unwrap();
        let security = SecurityPolicy {
            workspace_dir: workspace.path().to_path_buf(),
            workspace_only: true,
            ..SecurityPolicy::default()
        };
        let multimodal = MultimodalConfig::default();
        let history = vec![ChatMessage::user(format!(
            "look at this [IMAGE: {}]",
            image_path.display()
        ))];

        let err = resolve_vision_provider(
            None,
            &PlainNonVisionPrimary,
            &history,
            &multimodal,
            "primary",
            "primary-model",
            "primary-model",
            Some(&security),
        )
        .await
        .err()
        .expect("a policy-readable image marker on a non-vision provider must fail");

        let capability_error = err
            .downcast_ref::<ProviderCapabilityError>()
            .expect("vision refusal must retain its structured capability error");
        assert_eq!(capability_error.model_provider, "primary");
        assert_eq!(capability_error.capability, "vision");
        assert!(
            capability_error.message.contains("1 image marker(s)"),
            "the refusal must count the loadable marker: {capability_error}"
        );
    }

    /// `security: None` fails closed: even an existing file never counts, so
    /// a configless caller degrades instead of erroring.
    #[tokio::test]
    async fn no_policy_fails_closed_to_degrade() {
        struct PlainNonVisionPrimary;
        #[async_trait::async_trait]
        impl ModelProvider for PlainNonVisionPrimary {
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
        impl zeroclaw_api::attribution::Attributable for PlainNonVisionPrimary {
            fn role(&self) -> zeroclaw_api::attribution::Role {
                zeroclaw_api::attribution::Role::Provider(
                    zeroclaw_api::attribution::ProviderKind::Model(
                        zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "PlainNonVisionPrimary"
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("shot.png");
        std::fs::write(&image_path, b"an existing file nobody vouches for").unwrap();
        let multimodal = MultimodalConfig::default();
        let history = vec![ChatMessage::user(format!(
            "look at this [IMAGE: {}]",
            image_path.display()
        ))];

        let (vision_provider, degrade_strip_images) = resolve_vision_provider(
            None,
            &PlainNonVisionPrimary,
            &history,
            &multimodal,
            "primary",
            "primary-model",
            "primary-model",
            None,
        )
        .await
        .expect("no policy means fail closed: degrade, never fail the turn");
        assert!(
            vision_provider.is_none(),
            "no vision route exists, so the degrade branch must return None"
        );
        assert!(
            degrade_strip_images,
            "without a policy no local marker may count as resolvable"
        );
    }

    /// A vision-capable primary never reaches the resolvability branch:
    /// the turn routes to the primary unchanged, even with a policy-readable
    /// marker file in place. Pins the branch outcome (`(None, false)`, no
    /// degrade); the no-probe property is structural: the resolvable count
    /// exists only inside the no-vision, no-fallback `else`.
    #[tokio::test]
    async fn vision_capable_primary_takes_no_resolvability_branch() {
        struct VisionPrimary;
        #[async_trait::async_trait]
        impl ModelProvider for VisionPrimary {
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
                    vision: true,
                    ..Default::default()
                }
            }
        }
        impl zeroclaw_api::attribution::Attributable for VisionPrimary {
            fn role(&self) -> zeroclaw_api::attribution::Role {
                zeroclaw_api::attribution::Role::Provider(
                    zeroclaw_api::attribution::ProviderKind::Model(
                        zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "VisionPrimary"
            }
        }
        let workspace = tempfile::tempdir().unwrap();
        let image_path = workspace.path().join("shot.png");
        std::fs::write(&image_path, b"a readable file that must not matter").unwrap();
        let security = SecurityPolicy {
            workspace_dir: workspace.path().to_path_buf(),
            workspace_only: true,
            ..SecurityPolicy::default()
        };
        let multimodal = MultimodalConfig::default();
        let history = vec![ChatMessage::user(format!(
            "look at this [IMAGE: {}]",
            image_path.display()
        ))];

        let (vision_provider, degrade_strip_images) = resolve_vision_provider(
            None,
            &VisionPrimary,
            &history,
            &multimodal,
            "primary",
            "primary-model",
            "primary-model",
            Some(&security),
        )
        .await
        .expect("a vision-capable primary serves image turns itself");
        assert!(
            vision_provider.is_none(),
            "the primary is the route; no fallback provider is built"
        );
        assert!(
            !degrade_strip_images,
            "a vision-capable primary must not degrade/strip images"
        );
    }

    /// A configured vision fallback never reaches the resolvability branch
    /// either: the route builds the configured provider and returns it, and
    /// a policy-readable marker file is irrelevant to the decision. Same
    /// pinning caveat as above: the outcome proves the branch, the code
    /// placement proves the absence of probes.
    #[tokio::test]
    async fn configured_fallback_takes_no_resolvability_branch() {
        struct PlainNonVisionPrimary;
        #[async_trait::async_trait]
        impl ModelProvider for PlainNonVisionPrimary {
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
        impl zeroclaw_api::attribution::Attributable for PlainNonVisionPrimary {
            fn role(&self) -> zeroclaw_api::attribution::Role {
                zeroclaw_api::attribution::Role::Provider(
                    zeroclaw_api::attribution::ProviderKind::Model(
                        zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "PlainNonVisionPrimary"
            }
        }
        let workspace = tempfile::tempdir().unwrap();
        let image_path = workspace.path().join("shot.png");
        std::fs::write(&image_path, b"a readable file that must not matter").unwrap();
        let security = SecurityPolicy {
            workspace_dir: workspace.path().to_path_buf(),
            workspace_only: true,
            ..SecurityPolicy::default()
        };
        let config: Config = toml::from_str(
            r#"
schema_version = 3
[providers.models.custom.visionroute]
uri = "http://127.0.0.1:9/v1"
model = "vision-model"
"#,
        )
        .expect("config parses");
        let multimodal = MultimodalConfig {
            vision_model_provider: Some("custom.visionroute".to_string()),
            ..Default::default()
        };
        let history = vec![ChatMessage::user(format!(
            "look at this [IMAGE: {}]",
            image_path.display()
        ))];

        let (vision_provider, degrade_strip_images) = resolve_vision_provider(
            Some(&config),
            &PlainNonVisionPrimary,
            &history,
            &multimodal,
            "primary",
            "primary-model",
            "primary-model",
            Some(&security),
        )
        .await
        .expect("the configured vision fallback must build");
        let resolved =
            vision_provider.expect("the configured vision_model_provider must be returned");
        assert!(
            resolved
                .provider
                .capabilities_for_model(&resolved.model)
                .vision,
            "the configured fallback must be vision-capable"
        );
        assert!(
            !degrade_strip_images,
            "a live vision route must not degrade/strip images"
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
            "primary-model",
            None,
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
            "primary-model",
            None,
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
}
