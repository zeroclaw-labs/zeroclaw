//! Vision model-provider routing and per-iteration message preparation.

use anyhow::Result;
use zeroclaw_config::schema::MultimodalConfig;
use zeroclaw_providers::{ChatMessage, ModelProvider, ProviderCapabilityError, multimodal};

/// Resolve the vision route for this iteration.
///
/// Returns the on-demand vision provider (owned `Box`, never a borrow) and
/// the `degrade_strip_images` flag. The active (provider, name, model) triple
/// derivation stays inline in the loop (RUN_SHEET `turn.vision_route`).
pub(crate) fn resolve_vision_provider(
    model_provider: &dyn ModelProvider,
    history: &[ChatMessage],
    multimodal_config: &MultimodalConfig,
    provider_name: &str,
) -> Result<(Option<Box<dyn ModelProvider>>, bool)> {
    let image_marker_count = multimodal::count_image_markers(history);
    // Image markers in the most recent user message (the image the user *just*
    // sent this turn), as opposed to markers carried over from earlier history
    // or arriving via tool results. A missing vision capability is handled
    // differently: an image the user just sent must surface an error (we cannot
    // silently ignore it), while a carried-over or tool-result image degrades to
    // text-only. Scoping to the latest user message (rather than the whole
    // history) is what stops a single failed image turn from poisoning every
    // subsequent text turn: the marker lives in the long-lived session history
    // permanently, so a history-wide check would re-fail forever.
    let latest_user_image_marker_count = multimodal::count_latest_user_image_markers(history);

    // ── Vision model_provider routing ──────────────────────────
    // When the default model_provider lacks vision support but a dedicated
    // vision_model_provider is configured, create it on demand and use it
    // for this iteration. When no vision route exists at all, either
    // surface a capability error (the user just sent an image) or degrade
    // gracefully (the markers are carried over from earlier history or came
    // only from tool results); see the no-vision-route branch below and
    // `degrade_strip_images`.
    let mut degrade_strip_images = false;
    let vision_model_provider_box: Option<Box<dyn ModelProvider>> = if image_marker_count > 0
        && !model_provider.supports_vision()
    {
        if let Some(ref vp) = multimodal_config.vision_model_provider {
            let vp_instance = zeroclaw_providers::create_model_provider(vp, None).map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_category(::zeroclaw_log::EventCategory::Provider)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "vision_provider": vp,
                            "error": format!("{}", e),
                        })),
                    "vision model_provider construction failed"
                );
                anyhow::Error::msg(format!(
                    "failed to create vision model_provider '{vp}': {e}"
                ))
            })?;
            if !vp_instance.supports_vision() {
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
            Some(vp_instance)
        } else if latest_user_image_marker_count > 0 {
            // The user *just* sent an image we cannot see. Surface a capability
            // error so the attachment is not silently ignored — channels
            // render this back to the user (e.g. "⚠️ Error … does not
            // support vision"). Configuring a `vision_model_provider`
            // routes around it.
            return Err(ProviderCapabilityError {
                        model_provider: provider_name.to_string(),
                        capability: "vision".to_string(),
                        message: format!(
                            "received {latest_user_image_marker_count} image marker(s), but this model_provider does not support vision input"
                        ),
                    }
                    .into());
        } else {
            // The only image markers left are carried over from earlier
            // history (e.g. a prior failed image turn whose user message
            // persisted, or a switch from a vision model to a non-vision one)
            // or arrived via tool results (`image_info`, `screenshot`,
            // `image_gen`). Erroring here would poison every later turn: the
            // marker lives in the long-lived session history permanently, so a
            // history-wide capability error would re-fire on plain text turns
            // forever. Tool-result markers were already degraded for the same
            // "don't fail an otherwise useful turn" reason. Instead, degrade:
            // strip the markers from the messages sent to the text-only
            // provider while preserving the surrounding text, so the
            // conversation continues and the model still receives any
            // accompanying caption/metadata.
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
    } else {
        None
    };

    Ok((vision_model_provider_box, degrade_strip_images))
}

/// Prepare the iteration's outbound messages for the active provider.
///
/// When `image_cache` is `Some`, resolved local image data URIs are reused
/// across iterations and turns (embedded `Agent` paths pass the per-session
/// cache) so each file is read + base64-encoded at most once; channel/CLI
/// paths pass `None` and resolve fresh.
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
    // Fail closed before any provider sees the history. A no-user history
    // (session-only, or a leading assistant/tool block with no anchoring user
    // turn) sanitizes down to system-only, which every strict provider rejects
    // and which silently discards the only surviving non-system context.
    // Refuse to build a provider payload with zero user turns rather than
    // trade one strict-provider failure shape for another.
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

    /// Wiring check (#7415): the per-session `image_cache` threaded from the
    /// embedded `Agent` wrappers is populated on the first prep and reused on
    /// later iterations/turns, so a local image file is read + base64-encoded
    /// once instead of on every loop iteration. The `None` path (channels/CLI)
    /// still resolves correctly.
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

    /// Regression: a history whose first non-system turn is an assistant
    /// tool-call (context trim / restore decapitated the anchoring user turn)
    /// must be sanitized before any provider sees it, so strict providers no
    /// longer reject the leading tool-call turn.
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

    /// Fail-closed regression (#6302, "NO USER TURN AT ALL"): a history that
    /// sanitizes down to system-only must NOT produce a provider payload. The
    /// prep boundary returns an error before dispatch instead of sending a
    /// user-less request (or silently discarding the surviving context).
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
}
