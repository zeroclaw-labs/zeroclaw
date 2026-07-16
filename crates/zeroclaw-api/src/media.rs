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
            ext if is_image_extension(ext) => MediaKind::Image,
            "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" => MediaKind::Video,
            _ => MediaKind::Unknown,
        }
    }

    /// Conservative image check for security-sensitive consumers.
    ///
    /// [`kind()`](Self::kind) resolves each attachment to a single kind with
    /// MIME taking precedence, which is right for routing but wrong for a
    /// safety gate: MIME is sender-supplied, so a contradictory value (say
    /// `video/mp4` on a real photo) or a missing extension would let an image
    /// slip past a `kind() == Image` check. This method instead treats the
    /// attachment as an image when ANY signal says so: an `image/*` MIME, an
    /// image file extension, or image magic bytes in the payload. A false
    /// positive only over-applies image-turn restrictions, which is the safe
    /// direction.
    pub fn looks_like_image(&self) -> bool {
        if let Some(ref mime) = self.mime_type
            && mime.to_ascii_lowercase().starts_with("image/")
        {
            return true;
        }

        let ext = self
            .file_name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        if is_image_extension(&ext) {
            return true;
        }

        sniff_image_magic(&self.data)
    }
}

/// Shared image-extension list used by both [`MediaAttachment::kind`] and
/// [`MediaAttachment::looks_like_image`], so the two classifiers cannot
/// drift apart.
fn is_image_extension(ext: &str) -> bool {
    matches!(
        ext,
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "heic" | "tiff" | "svg"
    )
}

/// Detect common image formats from leading magic bytes.
fn sniff_image_magic(data: &[u8]) -> bool {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return true; // JPEG
    }
    if data.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        return true; // PNG
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return true; // GIF
    }
    if data.starts_with(b"BM") {
        return true; // BMP
    }
    if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        return true; // WebP
    }
    if data.starts_with(&[0x49, 0x49, 0x2A, 0x00]) || data.starts_with(&[0x4D, 0x4D, 0x00, 0x2A]) {
        return true; // TIFF (little/big endian)
    }
    // ISO BMFF (HEIC/HEIF/AVIF): "ftyp" box at offset 4 with an image brand.
    if data.len() >= 12 && &data[4..8] == b"ftyp" {
        let brand = &data[8..12];
        if matches!(
            brand,
            b"heic" | b"heix" | b"heif" | b"mif1" | b"msf1" | b"avif"
        ) {
            return true;
        }
    }
    false
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

    fn att_with_data(file_name: &str, mime_type: Option<&str>, data: &[u8]) -> MediaAttachment {
        MediaAttachment {
            file_name: file_name.to_string(),
            data: data.to_vec(),
            mime_type: mime_type.map(str::to_string),
        }
    }

    #[test]
    fn looks_like_image_accepts_any_single_signal() {
        // MIME alone (extensionless upload, sender-declared image type).
        assert!(att("upload", Some("image/jpeg")).looks_like_image());
        // Extension alone (no MIME available, as with Telegram photos).
        assert!(att("photo.jpg", None).looks_like_image());
        // Magic bytes alone (no MIME, no extension).
        assert!(att_with_data("upload", None, &[0xFF, 0xD8, 0xFF, 0xE0]).looks_like_image());
        assert!(
            att_with_data(
                "upload",
                None,
                &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
            )
            .looks_like_image()
        );
    }

    #[test]
    fn looks_like_image_is_not_negated_by_contradictory_mime() {
        // kind() would say Video here (MIME wins); the conservative check
        // must still flag the image extension so a spoofed MIME cannot dodge
        // image-turn restrictions.
        let a = att("photo.jpg", Some("video/mp4"));
        assert_eq!(a.kind(), MediaKind::Video);
        assert!(a.looks_like_image());
    }

    #[test]
    fn looks_like_image_rejects_non_images() {
        assert!(!att("doc.pdf", Some("application/pdf")).looks_like_image());
        assert!(!att("voice.ogg", Some("audio/ogg")).looks_like_image());
        assert!(!att_with_data("notes", None, b"plain text bytes").looks_like_image());
        assert!(!att("noextension", None).looks_like_image());
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
