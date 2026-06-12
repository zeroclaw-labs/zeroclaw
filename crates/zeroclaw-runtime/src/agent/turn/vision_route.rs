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
    // Image markers that came from the user (inbound attachments), as
    // opposed to tool results. A missing vision capability is handled
    // differently for the two: a user image must surface an error (we
    // cannot silently ignore what the user sent), while a tool-result
    // image may degrade to text-only.
    let user_image_marker_count = multimodal::count_user_image_markers(history);

    // ── Vision model_provider routing ──────────────────────────
    // When the default model_provider lacks vision support but a dedicated
    // vision_model_provider is configured, create it on demand and use it
    // for this iteration. When no vision route exists at all, either
    // surface a capability error (the user sent an image) or degrade
    // gracefully (the markers came only from tool results) — see the
    // no-vision-route branch below and `degrade_strip_images`.
    let mut degrade_strip_images = false;
    let vision_model_provider_box: Option<Box<dyn ModelProvider>> = if image_marker_count > 0
        && !model_provider.supports_vision()
    {
        if let Some(ref vp) = multimodal_config.vision_model_provider {
            let vp_instance = zeroclaw_providers::create_model_provider(vp, None).map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
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
        } else if user_image_marker_count > 0 {
            // The user sent an image we cannot see. Surface a capability
            // error so the attachment is not silently ignored — channels
            // render this back to the user (e.g. "⚠️ Error … does not
            // support vision"). Configuring a `vision_model_provider`
            // routes around it.
            return Err(ProviderCapabilityError {
                        model_provider: provider_name.to_string(),
                        capability: "vision".to_string(),
                        message: format!(
                            "received {image_marker_count} image marker(s), but this model_provider does not support vision input"
                        ),
                    }
                    .into());
        } else {
            // Markers came only from tool results (e.g. `image_info`,
            // `screenshot`, `image_gen`). Previously this aborted the
            // entire turn with a capability error, which turned an
            // otherwise successful tool call (e.g. `image_info`, which
            // always returns useful metadata text alongside its `[IMAGE:]`
            // marker) into a hard failure. Instead, degrade: strip the
            // image markers from the messages sent to the text-only
            // provider while preserving the surrounding text, so the
            // conversation continues and the model still receives any
            // accompanying metadata/caption.
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "model_provider": provider_name,
                        "image_marker_count": image_marker_count,
                    })),
                "no vision route for tool-result image marker(s); degrading to text-only (markers stripped)"
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
pub(crate) async fn prepare_messages_for_iteration(
    history: &[ChatMessage],
    multimodal_config: &MultimodalConfig,
    degrade_strip_images: bool,
) -> Result<multimodal::PreparedMessages> {
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
        multimodal::prepare_messages_for_provider(&stripped, multimodal_config).await
    } else {
        multimodal::prepare_messages_for_provider(history, multimodal_config).await
    }
}
