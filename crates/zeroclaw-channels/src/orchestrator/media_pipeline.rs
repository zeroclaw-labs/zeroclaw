//! Automatic media understanding pipeline for inbound channel messages.
//!
//! Pre-processes media attachments (audio, images, video) before the agent sees
//! the message, enriching the text with human-readable annotations:
//!
//! - **Audio**: transcribed via the existing [`super::transcription`] infrastructure,
//!   prepended as `[Audio transcription: ...]`.
//! - **Images**: when a vision-capable model_provider is active, described as `[Image: <description>]`.
//!   Falls back to `[Image: attached]` when vision is unavailable.
//! - **Video**: summarised as `[Video summary: ...]` when an API is available,
//!   otherwise `[Video: attached]`.
//!
//! The pipeline is **opt-in** via `[media_pipeline] enabled = true` in config.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::borrow::Cow;
use zeroclaw_config::schema::MediaPipelineConfig;

use super::super::transcription::TranscriptionManager;

// Re-export media types from zeroclaw-types for backwards compatibility.
pub use zeroclaw_api::media::{MediaAttachment, MediaKind};

/// The media understanding pipeline.
///
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
    ///
    /// If the pipeline is disabled via config, returns `original_text` unchanged.
    pub async fn process(&self, original_text: &str, attachments: &[MediaAttachment]) -> String {
        if !self.config.enabled || attachments.is_empty() {
            return original_text.to_string();
        }

        let mut annotations = Vec::new();

        for attachment in attachments {
            match attachment.kind() {
                MediaKind::Audio if self.config.transcribe_audio => {
                    let annotation = self.process_audio(attachment).await;
                    annotations.push(annotation);
                }
                MediaKind::Image if self.config.describe_images => {
                    // Channels that save the image to disk (Telegram, Discord)
                    // already emit a re-loadable `[IMAGE:<path>]` marker for
                    // this attachment in the message text. Adding a second,
                    // base64-inlined marker here would send the same image to
                    // the provider twice and persist megabytes of base64 into
                    // session history (the current turn is stored verbatim;
                    // only older turns get inline payloads collapsed, #3460).
                    if text_already_references_image(original_text, &attachment.file_name) {
                        continue;
                    }
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

    /// Describe an image attachment.
    ///
    /// When vision is available, the image will be passed through to the
    /// model_provider as an `[IMAGE:]` marker and described by the model in the
    /// normal flow.
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
    ///
    /// Video analysis requires external APIs not currently integrated.
    /// For now we add a placeholder annotation.
    fn process_video(&self, attachment: &MediaAttachment) -> String {
        format!("[Video: {} attached]", attachment.file_name)
    }
}

/// True when the message text already carries an `[IMAGE:<target>]` marker
/// whose target's final path component is exactly this attachment's file
/// name, i.e. the channel saved the image to disk and emitted a re-loadable
/// path marker itself. Each marker's target is parsed and compared by exact
/// basename (not substring-matched), so a caption that merely mentions the
/// file name next to an unrelated marker, or a marker for a differently
/// named file, cannot suppress a genuinely new attachment. The match is not
/// provenance-aware: a marker for a SAME-named file in another directory
/// would still count, which is acceptable because the channels that emit
/// these markers (Telegram, Discord) always add one for the attachment they
/// deliver in the same message.
fn text_already_references_image(text: &str, file_name: &str) -> bool {
    if file_name.is_empty() {
        return false;
    }
    let mut rest = text;
    while let Some(start) = rest.find("[IMAGE:") {
        let after = &rest[start + "[IMAGE:".len()..];
        let Some(end) = after.find(']') else {
            return false;
        };
        let target = &after[..end];
        let basename = target.rsplit(['/', '\\']).next().unwrap_or(target);
        if basename == file_name {
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
        }
    }

    fn sample_image() -> MediaAttachment {
        MediaAttachment {
            file_name: "photo.jpg".to_string(),
            data: vec![0u8; 50],
            mime_type: Some("image/jpeg".to_string()),
        }
    }

    fn sample_video() -> MediaAttachment {
        MediaAttachment {
            file_name: "clip.mp4".to_string(),
            data: vec![0u8; 200],
            mime_type: Some("video/mp4".to_string()),
        }
    }

    #[test]
    fn media_kind_from_mime() {
        let audio = MediaAttachment {
            file_name: "file".to_string(),
            data: vec![],
            mime_type: Some("audio/ogg".to_string()),
        };
        assert_eq!(audio.kind(), MediaKind::Audio);

        let image = MediaAttachment {
            file_name: "file".to_string(),
            data: vec![],
            mime_type: Some("image/png".to_string()),
        };
        assert_eq!(image.kind(), MediaKind::Image);

        let video = MediaAttachment {
            file_name: "file".to_string(),
            data: vec![],
            mime_type: Some("video/mp4".to_string()),
        };
        assert_eq!(video.kind(), MediaKind::Video);
    }

    #[test]
    fn media_kind_from_extension() {
        let audio = MediaAttachment {
            file_name: "voice.ogg".to_string(),
            data: vec![],
            mime_type: None,
        };
        assert_eq!(audio.kind(), MediaKind::Audio);

        let image = MediaAttachment {
            file_name: "photo.png".to_string(),
            data: vec![],
            mime_type: None,
        };
        assert_eq!(image.kind(), MediaKind::Image);

        let video = MediaAttachment {
            file_name: "clip.mp4".to_string(),
            data: vec![],
            mime_type: None,
        };
        assert_eq!(video.kind(), MediaKind::Video);

        let unknown = MediaAttachment {
            file_name: "data.bin".to_string(),
            data: vec![],
            mime_type: None,
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
        let result = pipeline.process(original, &[sample_image()]).await;
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

    #[tokio::test]
    async fn image_document_marked_by_channel_is_not_inlined() {
        let config = default_pipeline_config(true);
        let pipeline = MediaPipeline::new(&config, None, true);

        // An image sent "as file" (extensionless, image MIME): the channel
        // now emits the same [IMAGE:<path>] marker as for photos, so the
        // pipeline must not add a second, base64-inlined copy (#8965 review
        // round 3).
        let attachment = MediaAttachment {
            file_name: "upload".to_string(),
            data: vec![0xFF, 0xD8, 0xFF, 0xE0],
            mime_type: Some("image/jpeg".to_string()),
        };
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
    async fn caption_mentioning_file_name_does_not_suppress_new_attachment() {
        let config = default_pipeline_config(true);
        let pipeline = MediaPipeline::new(&config, None, true);

        // The caption mentions the new file's name and an unrelated marker is
        // present, but no marker actually targets photo.jpg. Substring
        // matching would wrongly skip here; precise marker parsing must not.
        let original = "[IMAGE:/workspace/telegram_files/old.png] compare with photo.jpg please";
        let result = pipeline.process(original, &[sample_image()]).await;
        assert!(
            result.contains("[IMAGE:data:image/jpeg;base64,"),
            "a caption mention must not count as a channel marker: {result}"
        );
    }

    #[test]
    fn text_already_references_image_matches_marker_targets_precisely() {
        // Exact basename match against a path marker.
        assert!(text_already_references_image(
            "[IMAGE:/ws/files/photo.jpg]",
            "photo.jpg"
        ));
        // Extensionless target (image sent as document).
        assert!(text_already_references_image(
            "[IMAGE:/ws/files/upload] caption",
            "upload"
        ));
        // Windows-style separators.
        assert!(text_already_references_image(
            r"[IMAGE:C:\ws\files\photo.jpg]",
            "photo.jpg"
        ));
        // Second marker matches after a non-matching first one.
        assert!(text_already_references_image(
            "[IMAGE:/ws/old.png] and [IMAGE:/ws/photo.jpg]",
            "photo.jpg"
        ));

        // Marker for a different file does not match.
        assert!(!text_already_references_image(
            "[IMAGE:/ws/old.png]",
            "photo.jpg"
        ));
        // File name in caption text only (no marker targeting it).
        assert!(!text_already_references_image(
            "[IMAGE:/ws/old.png] see photo.jpg",
            "photo.jpg"
        ));
        // Basename must match exactly, not as a suffix or prefix.
        assert!(!text_already_references_image(
            "[IMAGE:/ws/not_photo.jpg]",
            "photo.jpg"
        ));
        // Data-URI markers (pipeline-generated) never match a file name.
        assert!(!text_already_references_image(
            "[IMAGE:data:image/jpeg;base64,AAAA]",
            "photo.jpg"
        ));
        // Unterminated marker cannot match.
        assert!(!text_already_references_image(
            "[IMAGE:/ws/photo.jpg",
            "photo.jpg"
        ));
        // Empty file name never matches.
        assert!(!text_already_references_image("[IMAGE:/ws/photo.jpg]", ""));
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
