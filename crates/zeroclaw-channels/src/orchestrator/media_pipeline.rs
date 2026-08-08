//! Automatic media understanding pipeline for inbound channel messages.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::borrow::Cow;
use zeroclaw_config::schema::MediaPipelineConfig;

use super::super::transcription::TranscriptionManager;

// Re-export media types from zeroclaw-types for backwards compatibility.
pub use zeroclaw_api::media::{MediaAttachment, MediaKind};

/// The media understanding pipeline.
/// Consumes a message's text and attachments, returning enriched text with
/// media annotations prepended.
pub struct MediaPipeline<'a> {
    config: &'a MediaPipelineConfig,
    transcription_manager: Option<&'a TranscriptionManager>,
    vision_available: bool,
}

impl<'a> MediaPipeline<'a> {
    /// Create a new pipeline. `vision_available` indicates whether the current
    /// model provider supports vision (image description). `transcription_manager`
    /// is `None` when transcription is disabled at the channel level — audio
    /// attachments fall back to `[Audio: attached]` annotations.
    pub fn new(
        config: &'a MediaPipelineConfig,
        transcription_manager: Option<&'a TranscriptionManager>,
        vision_available: bool,
    ) -> Self {
        Self {
            config,
            transcription_manager,
            vision_available,
        }
    }

    /// Process a message's attachments and return enriched text.
    /// If the pipeline is disabled via config, returns `original_text` unchanged.
    pub async fn process(&self, original_text: &str, attachments: &[MediaAttachment]) -> String {
        if !self.config.enabled || attachments.is_empty() {
            return original_text.to_string();
        }

        let mut annotations = Vec::new();

        for attachment in attachments {
            // A channel that saved these bytes and rendered a re-loadable
            // `[IMAGE:<path>]` marker for them has already classified this
            // attachment, with more to go on than the pipeline has: the
            // payload, the sender's declared type, and the transport's own
            // notion of what was sent. That verdict wins outright.
            //
            // Skipping the whole attachment — not just its image branch — is
            // what keeps the two classifiers from contradicting each other.
            // `kind()` resolves a single kind with the declared MIME first, so
            // a `photo.jpg` sent as `video/mp4` routes to video here while the
            // channel marked it an image; annotating it would put an image
            // marker and a `[Video: ...]` note on one attachment. Deferring
            // also avoids a second, base64-inlined copy of an image the marker
            // already carries, which would send it to the provider twice and
            // persist megabytes of base64 into session history (the current
            // turn is stored verbatim; only older turns get inline payloads
            // collapsed).
            if text_already_references_image(original_text, attachment) {
                continue;
            }

            match attachment.kind() {
                MediaKind::Audio if self.config.transcribe_audio => {
                    let annotation = self.process_audio(attachment).await;
                    annotations.push(annotation);
                }
                MediaKind::Image if self.config.describe_images => {
                    let annotation = self.process_image(attachment);
                    annotations.push(annotation);
                }
                MediaKind::Video if self.config.summarize_video => {
                    let annotation = self.process_video(attachment);
                    annotations.push(annotation);
                }
                _ => {}
            }
        }

        if annotations.is_empty() {
            return original_text.to_string();
        }

        let mut enriched = String::with_capacity(
            annotations.iter().map(|a| a.len() + 1).sum::<usize>() + original_text.len() + 2,
        );

        for annotation in &annotations {
            enriched.push_str(annotation);
            enriched.push('\n');
        }

        if !original_text.is_empty() {
            enriched.push('\n');
            enriched.push_str(original_text);
        }

        enriched.trim().to_string()
    }

    /// Transcribe an audio attachment using the existing transcription infra.
    async fn process_audio(&self, attachment: &MediaAttachment) -> String {
        let Some(manager) = self.transcription_manager else {
            return "[Audio: attached]".to_string();
        };

        match manager
            .transcribe(&attachment.data, &attachment.file_name)
            .await
        {
            Ok(text) => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    "[Audio transcription: (empty)]".to_string()
                } else {
                    format!("[Audio transcription: {trimmed}]")
                }
            }
            Err(err) => {
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"file": attachment.file_name, "error": format!("{}", err)})), "Media pipeline: audio transcription failed");
                "[Audio: transcription failed]".to_string()
            }
        }
    }

    fn process_image(&self, attachment: &MediaAttachment) -> String {
        if self.vision_available {
            let (mime, data) = image_payload_for_vision(attachment);
            let b64 = STANDARD.encode(data.as_ref());
            format!(
                "[Image: {} attached, will be processed by vision model]\n[IMAGE:data:{};base64,{}]",
                attachment.file_name, mime, b64
            )
        } else {
            format!("[Image: {} attached]", attachment.file_name)
        }
    }

    /// Summarize a video attachment.
    /// Video analysis requires external APIs not currently integrated.
    /// For now we add a placeholder annotation.
    fn process_video(&self, attachment: &MediaAttachment) -> String {
        format!("[Video: {} attached]", attachment.file_name)
    }
}

/// True when the message text already carries an `[IMAGE:<target>]` marker
/// that the receiving channel rendered for **these exact bytes**.
///
/// The join is on [`MediaAttachment::marker_target`] — the target the channel
/// recorded when it rendered the marker — compared verbatim against each
/// marker in the text. Nothing is inferred from file names. That matters in
/// both directions:
///
/// * A channel whose on-disk name differs from the sender's name (Discord
///   prefixes a uniqueness token) is still recognized, so its marker is not
///   duplicated by a second base64-inlined copy.
/// * Sender-authored text cannot suppress a real attachment. A caption
///   containing `[IMAGE:/some/other/photo.jpg]` carries no channel
///   provenance, so it never matches, and the attachment's own annotation —
///   the only one holding the bytes — is still produced.
///
/// An attachment with no `marker_target` was supplied without the channel
/// referencing it in the text, so it is treated as unreferenced.
fn text_already_references_image(text: &str, attachment: &MediaAttachment) -> bool {
    let Some(target) = attachment.marker_target.as_deref() else {
        return false;
    };
    if target.is_empty() {
        return false;
    }

    let mut rest = text;
    while let Some(start) = rest.find("[IMAGE:") {
        let after = &rest[start + "[IMAGE:".len()..];
        let Some(end) = after.find(']') else {
            return false;
        };
        if &after[..end] == target {
            return true;
        }
        rest = &after[end + 1..];
    }
    false
}

fn image_payload_for_vision(attachment: &MediaAttachment) -> (String, Cow<'_, [u8]>) {
    let mime = attachment.mime_type.as_deref().unwrap_or("image/jpeg");

    #[cfg(feature = "image-normalization")]
    if is_webp_attachment(attachment, mime) {
        match webp_to_png(&attachment.data) {
            Ok(png) => return ("image/png".to_string(), Cow::Owned(png)),
            Err(err) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "file": attachment.file_name,
                            "error": format!("{}", err),
                            "error_key": "media_pipeline_webp_to_png_failed",
                        })),
                    "Media pipeline: failed to normalize WebP image for vision"
                );
            }
        }
    }

    (mime.to_string(), Cow::Borrowed(&attachment.data))
}

#[cfg(feature = "image-normalization")]
fn is_webp_attachment(attachment: &MediaAttachment, mime: &str) -> bool {
    mime.eq_ignore_ascii_case("image/webp")
        || attachment
            .file_name
            .rsplit_once('.')
            .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("webp"))
}

#[cfg(feature = "image-normalization")]
fn webp_to_png(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let image = image::load_from_memory_with_format(data, image::ImageFormat::WebP)?;
    let mut cursor = std::io::Cursor::new(Vec::new());
    image.write_to(&mut cursor, image::ImageFormat::Png)?;
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_pipeline_config(enabled: bool) -> MediaPipelineConfig {
        MediaPipelineConfig {
            enabled,
            transcribe_audio: true,
            describe_images: true,
            summarize_video: true,
        }
    }

    fn sample_audio() -> MediaAttachment {
        MediaAttachment {
            file_name: "voice.ogg".to_string(),
            data: vec![0u8; 100],
            mime_type: Some("audio/ogg".to_string()),
            marker_target: None,
        }
    }

    fn sample_image() -> MediaAttachment {
        MediaAttachment {
            file_name: "photo.jpg".to_string(),
            data: vec![0u8; 50],
            mime_type: Some("image/jpeg".to_string()),
            marker_target: None,
        }
    }

    fn sample_video() -> MediaAttachment {
        MediaAttachment {
            file_name: "clip.mp4".to_string(),
            data: vec![0u8; 200],
            mime_type: Some("video/mp4".to_string()),
            marker_target: None,
        }
    }

    #[test]
    fn media_kind_from_mime() {
        let audio = MediaAttachment {
            file_name: "file".to_string(),
            data: vec![],
            mime_type: Some("audio/ogg".to_string()),
            marker_target: None,
        };
        assert_eq!(audio.kind(), MediaKind::Audio);

        let image = MediaAttachment {
            file_name: "file".to_string(),
            data: vec![],
            mime_type: Some("image/png".to_string()),
            marker_target: None,
        };
        assert_eq!(image.kind(), MediaKind::Image);

        let video = MediaAttachment {
            file_name: "file".to_string(),
            data: vec![],
            mime_type: Some("video/mp4".to_string()),
            marker_target: None,
        };
        assert_eq!(video.kind(), MediaKind::Video);
    }

    #[test]
    fn media_kind_from_extension() {
        let audio = MediaAttachment {
            file_name: "voice.ogg".to_string(),
            data: vec![],
            mime_type: None,
            marker_target: None,
        };
        assert_eq!(audio.kind(), MediaKind::Audio);

        let image = MediaAttachment {
            file_name: "photo.png".to_string(),
            data: vec![],
            mime_type: None,
            marker_target: None,
        };
        assert_eq!(image.kind(), MediaKind::Image);

        let video = MediaAttachment {
            file_name: "clip.mp4".to_string(),
            data: vec![],
            mime_type: None,
            marker_target: None,
        };
        assert_eq!(video.kind(), MediaKind::Video);

        let unknown = MediaAttachment {
            file_name: "data.bin".to_string(),
            data: vec![],
            mime_type: None,
            marker_target: None,
        };
        assert_eq!(unknown.kind(), MediaKind::Unknown);
    }

    #[tokio::test]
    async fn disabled_pipeline_returns_original_text() {
        let config = default_pipeline_config(false);
        let pipeline = MediaPipeline::new(&config, None, false);

        let result = pipeline.process("hello", &[sample_audio()]).await;
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn empty_attachments_returns_original_text() {
        let config = default_pipeline_config(true);
        let pipeline = MediaPipeline::new(&config, None, false);

        let result = pipeline.process("hello", &[]).await;
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn image_annotation_with_vision() {
        let config = default_pipeline_config(true);
        let pipeline = MediaPipeline::new(&config, None, true);

        let result = pipeline.process("check this", &[sample_image()]).await;
        assert!(
            result.contains("[Image: photo.jpg attached, will be processed by vision model]"),
            "expected vision annotation, got: {result}"
        );
        assert!(
            result.contains("[IMAGE:data:image/jpeg;base64,"),
            "expected image data marker, got: {result}"
        );
        assert!(result.contains("check this"));
    }

    #[tokio::test]
    async fn image_already_marked_by_channel_is_not_double_described() {
        let config = default_pipeline_config(true);
        let pipeline = MediaPipeline::new(&config, None, true);

        // A channel (Telegram, Discord) that saved the file to disk emits a
        // re-loadable path marker itself; the pipeline must not add a second,
        // base64-inlined copy of the same image.
        let original = "[IMAGE:/workspace/telegram_files/photo.jpg]\n\nlog this automatically";
        let attachment = marked(
            "photo.jpg",
            "image/jpeg",
            "/workspace/telegram_files/photo.jpg",
        );
        let result = pipeline.process(original, &[attachment]).await;
        assert_eq!(
            result, original,
            "pre-marked image must pass through unchanged"
        );
        assert!(
            !result.contains("base64"),
            "no inline base64 may be added for a pre-marked image: {result}"
        );
    }

    #[tokio::test]
    async fn unrelated_image_marker_does_not_suppress_new_attachment() {
        let config = default_pipeline_config(true);
        let pipeline = MediaPipeline::new(&config, None, true);

        // A quoted older image marker for a DIFFERENT file must not swallow
        // the annotation for the newly attached one.
        let original = "[IMAGE:/workspace/telegram_files/old_photo.png] earlier pic";
        let result = pipeline.process(original, &[sample_image()]).await;
        assert!(
            result.contains("[IMAGE:data:image/jpeg;base64,"),
            "new attachment must still be annotated: {result}"
        );
    }

    /// An attachment as a channel hands it over: bytes plus the target the
    /// channel already rendered for them.
    fn marked(file_name: &str, mime: &str, marker_target: &str) -> MediaAttachment {
        MediaAttachment {
            file_name: file_name.to_string(),
            data: vec![0xFF, 0xD8, 0xFF, 0xE0],
            mime_type: Some(mime.to_string()),
            marker_target: Some(marker_target.to_string()),
        }
    }

    #[tokio::test]
    async fn image_document_marked_by_channel_is_not_inlined() {
        let config = default_pipeline_config(true);
        let pipeline = MediaPipeline::new(&config, None, true);

        // An image sent "as file" (extensionless, image MIME): the channel
        // emits the same [IMAGE:<path>] marker as for photos, so the pipeline
        // must not add a second, base64-inlined copy.
        let attachment = marked("upload", "image/jpeg", "/workspace/telegram_files/upload");
        let original = "[IMAGE:/workspace/telegram_files/upload]\n\nplease describe";
        let result = pipeline.process(original, &[attachment]).await;
        assert_eq!(
            result, original,
            "channel-marked image document must pass through unchanged"
        );
        assert!(
            !result.contains("IMAGE:data:"),
            "no inline base64 may be added for a channel-marked image document: {result}"
        );
    }

    #[tokio::test]
    async fn discord_uuid_prefixed_marker_is_recognized_as_its_own_attachment() {
        let config = default_pipeline_config(true);
        let pipeline = MediaPipeline::new(&config, None, true);

        // Discord saves under a uniqueness-prefixed name while the envelope
        // keeps the sender's name, so the two never share a basename. Joining
        // on the rendered target is what keeps this single-copy.
        let saved = "/ws/discord_files/6f1e4a4c-2b77-4a2f-9d0e-5c1f0b3a7e11_photo.jpg";
        let attachment = marked("photo.jpg", "image/jpeg", saved);
        let original = format!("[IMAGE:{saved}]\n\nwhat is this?");

        let result = pipeline.process(&original, &[attachment]).await;

        assert_eq!(
            result, original,
            "a Discord-saved image must not be inlined a second time"
        );
        assert!(
            !result.contains("IMAGE:data:"),
            "uuid-prefixed save names must still join to their marker: {result}"
        );
    }

    #[tokio::test]
    async fn sender_authored_marker_cannot_suppress_a_real_attachment() {
        let config = default_pipeline_config(true);
        let pipeline = MediaPipeline::new(&config, None, true);

        // The sender typed a marker naming the same basename as the real
        // attachment, but pointing somewhere else entirely. Only the channel's
        // own target counts, so the byte-backed annotation is still produced.
        let attachment = marked("photo.jpg", "image/jpeg", "/ws/telegram_files/photo.jpg");
        let original = "[IMAGE:/ws/telegram_files/old/photo.jpg] describe the attached one";

        let result = pipeline.process(original, &[attachment]).await;

        assert!(
            result.contains("[IMAGE:data:image/jpeg;base64,"),
            "a sender-authored marker must not drop the only copy of the bytes: {result}"
        );
    }

    #[tokio::test]
    async fn contradictory_signals_cannot_produce_contradictory_annotations() {
        let config = default_pipeline_config(true);
        let pipeline = MediaPipeline::new(&config, None, true);

        // `kind()` reads the declared MIME and says video; the channel read
        // the name and the payload and marked it an image. One attachment
        // must not end up with both an image marker and a video note.
        let attachment = MediaAttachment {
            file_name: "photo.jpg".to_string(),
            data: vec![0xFF, 0xD8, 0xFF, 0xE0],
            mime_type: Some("video/mp4".to_string()),
            marker_target: Some("/ws/telegram_files/photo.jpg".to_string()),
        };
        assert_eq!(
            attachment.kind(),
            MediaKind::Video,
            "this test is only meaningful while the declared MIME wins routing"
        );

        let original = "[IMAGE:/ws/telegram_files/photo.jpg]\n\nwhat is this?";
        let result = pipeline.process(original, &[attachment]).await;

        assert_eq!(
            result, original,
            "the channel's rendered verdict must stand alone"
        );
        assert!(
            !result.contains("[Video:"),
            "an image-marked attachment must not also be annotated as video: {result}"
        );
    }

    #[tokio::test]
    async fn unmarked_attachment_is_always_annotated() {
        let config = default_pipeline_config(true);
        let pipeline = MediaPipeline::new(&config, None, true);

        // A channel that supplies bytes without rendering a marker gets the
        // pipeline's annotation even when the text mentions a same-named file.
        let original = "[IMAGE:/ws/telegram_files/photo.jpg] and also photo.jpg";
        let result = pipeline.process(original, &[sample_image()]).await;

        assert!(
            result.contains("[IMAGE:data:image/jpeg;base64,"),
            "an attachment with no channel marker must be annotated: {result}"
        );
    }

    #[test]
    fn text_already_references_image_joins_on_the_channel_target() {
        let att = |target: Option<&str>| MediaAttachment {
            file_name: "photo.jpg".to_string(),
            data: Vec::new(),
            mime_type: Some("image/jpeg".to_string()),
            marker_target: target.map(str::to_string),
        };

        // Exact match against the target the channel recorded.
        assert!(text_already_references_image(
            "[IMAGE:/ws/files/photo.jpg]",
            &att(Some("/ws/files/photo.jpg"))
        ));
        // Second marker matches after a non-matching first one.
        assert!(text_already_references_image(
            "[IMAGE:/ws/old.png] and [IMAGE:/ws/photo.jpg]",
            &att(Some("/ws/photo.jpg"))
        ));
        // A URL target (no workspace configured) joins the same way.
        assert!(text_already_references_image(
            "[IMAGE:https://cdn.example/attachments/1/photo.jpg]",
            &att(Some("https://cdn.example/attachments/1/photo.jpg"))
        ));

        // Same basename, different directory: not this attachment.
        assert!(!text_already_references_image(
            "[IMAGE:/ws/old/photo.jpg]",
            &att(Some("/ws/files/photo.jpg"))
        ));
        // A prefix of the target is not the target.
        assert!(!text_already_references_image(
            "[IMAGE:/ws/files/photo.jp]",
            &att(Some("/ws/files/photo.jpg"))
        ));
        // Marker present only as caption prose.
        assert!(!text_already_references_image(
            "see /ws/files/photo.jpg",
            &att(Some("/ws/files/photo.jpg"))
        ));
        // Data-URI markers are the pipeline's own output, never a channel target.
        assert!(!text_already_references_image(
            "[IMAGE:data:image/jpeg;base64,AAAA]",
            &att(Some("/ws/files/photo.jpg"))
        ));
        // Unterminated marker cannot match.
        assert!(!text_already_references_image(
            "[IMAGE:/ws/files/photo.jpg",
            &att(Some("/ws/files/photo.jpg"))
        ));
        // No recorded target: the attachment is unreferenced by definition.
        assert!(!text_already_references_image(
            "[IMAGE:/ws/files/photo.jpg]",
            &att(None)
        ));
        // An empty target never matches.
        assert!(!text_already_references_image("[IMAGE:]", &att(Some(""))));
    }

    #[cfg(feature = "image-normalization")]
    #[tokio::test]
    async fn webp_image_is_normalized_to_png_for_vision() {
        let config = default_pipeline_config(true);
        let pipeline = MediaPipeline::new(&config, None, true);
        let mut cursor = std::io::Cursor::new(Vec::new());
        let webp = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([255, 0, 0, 255]),
        ));
        webp.write_to(&mut cursor, image::ImageFormat::WebP)
            .expect("test WebP should encode");

        let sticker = MediaAttachment {
            file_name: "sticker.webp".to_string(),
            data: cursor.into_inner(),
            mime_type: Some("image/webp".to_string()),
            marker_target: None,
        };

        let result = pipeline.process("what is this?", &[sticker]).await;

        assert!(result.contains("[IMAGE:data:image/png;base64,"));
        assert!(!result.contains("[IMAGE:data:image/webp;base64,"));
        assert!(result.contains("what is this?"));
    }

    #[tokio::test]
    async fn image_annotation_without_vision() {
        let config = default_pipeline_config(true);
        let pipeline = MediaPipeline::new(&config, None, false);

        let result = pipeline.process("check this", &[sample_image()]).await;
        assert!(
            result.contains("[Image: photo.jpg attached]"),
            "expected basic image annotation, got: {result}"
        );
        assert!(
            !result.contains("[IMAGE:data:"),
            "non-vision path must not inline image data, got: {result}"
        );
    }

    #[tokio::test]
    async fn video_annotation() {
        let config = default_pipeline_config(true);
        let pipeline = MediaPipeline::new(&config, None, false);

        let result = pipeline.process("watch", &[sample_video()]).await;
        assert!(
            result.contains("[Video: clip.mp4 attached]"),
            "expected video annotation, got: {result}"
        );
    }

    #[tokio::test]
    async fn audio_without_transcription_enabled() {
        let config = default_pipeline_config(true);
        let pipeline = MediaPipeline::new(&config, None, false);

        let result = pipeline.process("", &[sample_audio()]).await;
        assert_eq!(result, "[Audio: attached]");
    }

    #[tokio::test]
    async fn multiple_attachments_produce_multiple_annotations() {
        let config = default_pipeline_config(true);
        let pipeline = MediaPipeline::new(&config, None, false);

        let attachments = vec![sample_audio(), sample_image(), sample_video()];
        let result = pipeline.process("context", &attachments).await;

        assert!(
            result.contains("[Audio: attached]"),
            "missing audio annotation"
        );
        assert!(
            result.contains("[Image: photo.jpg attached]"),
            "missing image annotation"
        );
        assert!(
            result.contains("[Video: clip.mp4 attached]"),
            "missing video annotation"
        );
        assert!(result.contains("context"), "missing original text");
    }

    #[tokio::test]
    async fn disabled_sub_features_skip_processing() {
        let config = MediaPipelineConfig {
            enabled: true,
            transcribe_audio: false,
            describe_images: false,
            summarize_video: false,
        };
        let pipeline = MediaPipeline::new(&config, None, false);

        let attachments = vec![sample_audio(), sample_image(), sample_video()];
        let result = pipeline.process("hello", &attachments).await;
        assert_eq!(result, "hello");
    }
}
