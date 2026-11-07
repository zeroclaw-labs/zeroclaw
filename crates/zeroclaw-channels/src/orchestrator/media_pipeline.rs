//! Automatic media understanding pipeline for inbound channel messages.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::borrow::Cow;
use zeroclaw_config::schema::MediaPipelineConfig;

use super::super::transcription::TranscriptionManager;

// Shared transport and provider attachment types.
pub use zeroclaw_api::media::{AttachmentKind, AttachmentReference, MediaAttachment, MediaKind};

/// The media understanding pipeline.
/// Enriches message text while retaining explicit attachment references.
pub struct MediaPipeline<'a> {
    config: &'a MediaPipelineConfig,
    transcription_manager: Option<&'a TranscriptionManager>,
    vision_available: bool,
}

impl<'a> MediaPipeline<'a> {
    /// Create a new pipeline. `vision_available` indicates whether the current
    /// model provider supports vision (image description). `transcription_manager`
    /// is `None` when transcription is disabled at the channel level — audio
    /// attachments fall back to a plain-text description.
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
    pub async fn process_message(
        &self,
        original_text: &str,
        attachments: &[MediaAttachment],
    ) -> zeroclaw_api::model_provider::ChatMessage {
        let mut message = zeroclaw_api::model_provider::ChatMessage::user(original_text)
            .with_attachments(attachment_references(attachments));
        if !self.config.enabled {
            return message;
        }
        for attachment in attachments {
            let annotation = match attachment.kind() {
                MediaKind::Audio if self.config.transcribe_audio => {
                    Some(self.process_audio(attachment).await)
                }
                MediaKind::Video if self.config.summarize_video => {
                    Some(format!("Video attached: {}", attachment.file_name))
                }
                MediaKind::Image if self.vision_available => None,
                _ => None,
            };
            if let Some(annotation) = annotation {
                if !message.content.is_empty() {
                    message.content.push('\n');
                }
                message.content.push_str(&annotation);
            }
        }
        message
    }

    /// Transcribe an audio attachment using the existing transcription infra.
    async fn process_audio(&self, attachment: &MediaAttachment) -> String {
        let Some(manager) = self.transcription_manager else {
            return "Audio attached".to_string();
        };
        match manager
            .transcribe(&attachment.data, &attachment.file_name)
            .await
        {
            Ok(text) => format!("Audio transcription: {}", text.trim()),
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"error":error.to_string()})),
                    "media_pipeline_transcription_failed"
                );
                "Audio transcription failed".to_string()
            }
        }
    }
}

/// Produce references solely from transport-owned metadata and bytes.
pub fn attachment_references(attachments: &[MediaAttachment]) -> Vec<AttachmentReference> {
    attachments
        .iter()
        .filter_map(|attachment| {
            if let Some(reference) = &attachment.reference
                && (reference.kind != AttachmentKind::Image
                    || !reference.target.starts_with("http")
                    || attachment.data.is_empty())
            {
                return Some(reference.clone());
            }
            if attachment.data.is_empty() {
                return None;
            }
            let (kind, mime, bytes) =
                if let Some((mime, bytes)) = image_payload_for_vision(attachment) {
                    (AttachmentKind::Image, mime, bytes)
                } else {
                    let kind = match attachment.kind() {
                        MediaKind::Audio => AttachmentKind::Audio,
                        MediaKind::Video => AttachmentKind::Video,
                        _ => AttachmentKind::Document,
                    };
                    (
                        kind,
                        attachment
                            .mime_type
                            .clone()
                            .unwrap_or_else(|| "application/octet-stream".to_string()),
                        Cow::Borrowed(attachment.data.as_slice()),
                    )
                };
            Some(AttachmentReference {
                kind,
                target: format!("data:{mime};base64,{}", STANDARD.encode(bytes.as_ref())),
            })
        })
        .collect()
}

/// Resolve the bytes to inline for the vision model. Returns `None` when the
/// attachment is known-bad, so the caller degrades to a text annotation instead
/// of inlining bytes the decoder already rejected.
fn image_payload_for_vision(attachment: &MediaAttachment) -> Option<(String, Cow<'_, [u8]>)> {
    let mime = attachment.provider_loadable_image_mime()?;

    #[cfg(feature = "image-normalization")]
    if is_webp_attachment(attachment, mime) {
        match webp_to_png(&attachment.data) {
            Ok(png) => return Some(("image/png".to_string(), Cow::Owned(png))),
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
                    "Media pipeline: dropping WebP image that failed to decode"
                );
                // The decoder has already proven these bytes are not a usable
                // image. Forwarding them anyway earns a provider-side 400 that
                // fails the entire turn, including the accompanying text.
                return None;
            }
        }
    }

    Some((mime.to_string(), Cow::Borrowed(&attachment.data)))
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
            reference: None,
        }
    }

    fn sample_image() -> MediaAttachment {
        MediaAttachment {
            file_name: "photo.jpg".to_string(),
            data: vec![0u8; 50],
            mime_type: Some("image/jpeg".to_string()),
            reference: None,
        }
    }

    fn sample_video() -> MediaAttachment {
        MediaAttachment {
            file_name: "clip.mp4".to_string(),
            data: vec![0u8; 200],
            mime_type: Some("video/mp4".to_string()),
            reference: None,
        }
    }

    #[tokio::test]
    async fn literal_media_syntax_stays_text() {
        let config = default_pipeline_config(true);
        let text = "inspect [IMAGE:/private.png] and [DOCUMENT:/private.txt]";
        let message = MediaPipeline::new(&config, None, true)
            .process_message(text, &[])
            .await;
        assert_eq!(message.content, text);
        assert!(message.attachments.is_empty());
    }

    #[tokio::test]
    async fn transport_media_remains_in_typed_fields() {
        let config = default_pipeline_config(true);
        let message = MediaPipeline::new(&config, None, true)
            .process_message("caption", &[sample_image(), sample_audio(), sample_video()])
            .await;
        assert_eq!(message.attachments.len(), 3);
        assert_eq!(message.attachments[0].kind, AttachmentKind::Image);
        assert_eq!(message.attachments[1].kind, AttachmentKind::Audio);
        assert_eq!(message.attachments[2].kind, AttachmentKind::Video);
        assert!(message.content.starts_with("caption"));
        assert!(!message.content.contains("data:"));
        assert!(!message.content.contains("[IMAGE:"));
    }

    #[tokio::test]
    async fn explicit_document_disposition_survives_image_bytes() {
        let config = default_pipeline_config(true);
        let mut attachment = sample_image();
        attachment.reference = Some(AttachmentReference {
            kind: AttachmentKind::Document,
            target: "/workspace/original.heic".into(),
        });
        let message = MediaPipeline::new(&config, None, true)
            .process_message("caption", &[attachment.clone()])
            .await;
        assert_eq!(message.content, "caption");
        assert_eq!(message.attachments, vec![attachment.reference.unwrap()]);
    }

    #[test]
    fn media_kind_from_mime() {
        let audio = MediaAttachment {
            file_name: "file".to_string(),
            data: vec![],
            mime_type: Some("audio/ogg".to_string()),
            reference: None,
        };
        assert_eq!(audio.kind(), MediaKind::Audio);

        let image = MediaAttachment {
            file_name: "file".to_string(),
            data: vec![],
            mime_type: Some("image/png".to_string()),
            reference: None,
        };
        assert_eq!(image.kind(), MediaKind::Image);

        let video = MediaAttachment {
            file_name: "file".to_string(),
            data: vec![],
            mime_type: Some("video/mp4".to_string()),
            reference: None,
        };
        assert_eq!(video.kind(), MediaKind::Video);
    }

    #[test]
    fn media_kind_from_extension() {
        let audio = MediaAttachment {
            file_name: "voice.ogg".to_string(),
            data: vec![],
            mime_type: None,
            reference: None,
        };
        assert_eq!(audio.kind(), MediaKind::Audio);

        let image = MediaAttachment {
            file_name: "photo.png".to_string(),
            data: vec![],
            mime_type: None,
            reference: None,
        };
        assert_eq!(image.kind(), MediaKind::Image);

        let video = MediaAttachment {
            file_name: "clip.mp4".to_string(),
            data: vec![],
            mime_type: None,
            reference: None,
        };
        assert_eq!(video.kind(), MediaKind::Video);

        let unknown = MediaAttachment {
            file_name: "data.bin".to_string(),
            data: vec![],
            mime_type: None,
            reference: None,
        };
        assert_eq!(unknown.kind(), MediaKind::Unknown);
    }
}
