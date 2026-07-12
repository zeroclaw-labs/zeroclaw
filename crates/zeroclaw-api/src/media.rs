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
    /// Load an attachment from a file path on disk.
    ///
    /// # Caller path-validation contract
    ///
    /// This method reads the path supplied by the caller verbatim.  **Callers
    /// are responsible for validating or constraining `path` before calling
    /// this function when the path originates from untrusted input** (e.g. a
    /// user message, an HTTP request body, or any external data source).  No
    /// sandboxing or path canonicalization is performed here.
    ///
    /// Read errors are propagated as `Err` rather than silently producing an
    /// empty attachment, so the caller can decide how to handle missing or
    /// unreadable files.
    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let p = std::path::Path::new(path);
        let data = std::fs::read(p)?;
        let file_name = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("attachment")
            .to_string();
        let mime_type = match p.extension().and_then(|e| e.to_str()) {
            Some("pdf") => Some("application/pdf".to_string()),
            Some("xlsx") => Some(
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string(),
            ),
            Some("docx") => Some(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                    .to_string(),
            ),
            Some("csv") => Some("text/csv".to_string()),
            Some("png") => Some("image/png".to_string()),
            Some("jpg") | Some("jpeg") => Some("image/jpeg".to_string()),
            Some("txt") => Some("text/plain".to_string()),
            _ => Some("application/octet-stream".to_string()),
        };
        Ok(Self {
            file_name,
            data,
            mime_type,
        })
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn att(file_name: &str, mime_type: Option<&str>) -> MediaAttachment {
        MediaAttachment {
            file_name: file_name.to_string(),
            data: Vec::new(),
            mime_type: mime_type.map(str::to_string),
        }
    }

    #[test]
    fn kind_prefers_mime_type_over_extension() {
        // A known media MIME type wins even when the extension says otherwise.
        assert_eq!(att("photo.jpg", Some("audio/ogg")).kind(), MediaKind::Audio);
    }

    #[test]
    fn kind_mime_match_is_case_insensitive() {
        assert_eq!(att("x", Some("IMAGE/PNG")).kind(), MediaKind::Image);
        assert_eq!(att("x", Some("Video/MP4")).kind(), MediaKind::Video);
    }

    #[test]
    fn kind_falls_back_to_extension_when_mime_uninformative() {
        // octet-stream is not audio/image/video, so the extension decides.
        assert_eq!(
            att("voice.mp3", Some("application/octet-stream")).kind(),
            MediaKind::Audio
        );
    }

    #[test]
    fn kind_classifies_by_extension_when_no_mime() {
        let cases = [
            ("voice.ogg", MediaKind::Audio),
            ("song.FLAC", MediaKind::Audio),
            ("photo.jpeg", MediaKind::Image),
            ("pic.HEIC", MediaKind::Image),
            ("clip.mp4", MediaKind::Video),
            ("movie.mkv", MediaKind::Video),
            ("doc.pdf", MediaKind::Unknown),
            ("data.bin", MediaKind::Unknown),
            ("noextension", MediaKind::Unknown),
        ];
        for (name, want) in cases {
            assert_eq!(att(name, None).kind(), want, "{name}");
        }
    }

    #[test]
    fn from_file_reads_data_and_maps_extension_to_mime() {
        let path = std::env::temp_dir().join("zeroclaw_media_kind_test_sample.png");
        std::fs::write(&path, b"\x89PNG fake-bytes").unwrap();
        let att = MediaAttachment::from_file(path.to_str().unwrap()).unwrap();
        assert_eq!(att.file_name, "zeroclaw_media_kind_test_sample.png");
        assert_eq!(att.mime_type.as_deref(), Some("image/png"));
        assert_eq!(att.data, b"\x89PNG fake-bytes");
        assert_eq!(att.kind(), MediaKind::Image);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn from_file_propagates_read_error_for_missing_path() {
        let missing = std::env::temp_dir().join("zeroclaw_media_kind_missing_xyz.bin");
        let _ = std::fs::remove_file(&missing);
        assert!(MediaAttachment::from_file(missing.to_str().unwrap()).is_err());
    }
}
