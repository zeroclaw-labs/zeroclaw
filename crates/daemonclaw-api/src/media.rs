/// Classifies an attachment by MIME type or file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Audio,
    Image,
    Video,
    Unknown,
}

/// A single media attachment on an inbound message.
#[derive(Debug, Clone)]
pub struct MediaAttachment {
    /// Original file name (e.g. `voice.ogg`, `photo.jpg`).
    pub file_name: String,
    /// Raw bytes of the attachment.
    pub data: Vec<u8>,
    /// MIME type if known (e.g. `audio/ogg`, `image/jpeg`).
    pub mime_type: Option<String>,
}

impl MediaAttachment {
    /// Classify this attachment into a [`MediaKind`].
    pub fn kind(&self) -> MediaKind {
        // Try MIME type first.
        if let Some(ref mime) = self.mime_type {
            let lower = mime.to_ascii_lowercase();
            if lower.starts_with("audio/") {
                return MediaKind::Audio;
            }
            if lower.starts_with("image/") {
                return MediaKind::Image;
            }
            if lower.starts_with("video/") {
                return MediaKind::Video;
            }
        }

        // Fall back to file extension.
        let ext = self
            .file_name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();

        match ext.as_str() {
            "flac" | "mp3" | "mpeg" | "mpga" | "m4a" | "ogg" | "oga" | "opus" | "wav" | "webm" => {
                MediaKind::Audio
            }
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "heic" | "tiff" | "svg" => {
                MediaKind::Image
            }
            "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" => MediaKind::Video,
            _ => MediaKind::Unknown,
        }
    }
}
