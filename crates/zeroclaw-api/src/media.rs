//! Inbound media attachments and the three questions callers ask about them.
//!
//! An attachment carries several signals — a sender-declared MIME, a file
//! name, and the payload itself — and they can disagree. Rather than force one
//! verdict on every caller, this module exposes three deliberately different
//! answers, each matched to what its consumer does with it:
//!
//! * [`MediaAttachment::kind`] — routing. Resolves one kind, declared MIME
//!   first, so an attachment is processed as whatever the sender said it is.
//! * [`MediaAttachment::looks_like_image`] — restriction. True when *any*
//!   signal says image, so a contradictory MIME cannot smuggle a photo past an
//!   image-turn gate. Over-applies rather than under-applies, because its
//!   consumers only remove capability.
//! * [`MediaAttachment::provider_loadable_image_mime`] — grant. `Some` only
//!   when the multimodal loader will actually accept the bytes, so nothing
//!   promises the provider an image it will drop.
//!
//! The three are ordered by strictness (`provider_loadable_image_mime` implies
//! `looks_like_image`), but `kind` is independent and may disagree with both.
use serde::{Deserialize, Serialize};

/// Classifies an attachment by MIME type or file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Audio,
    Image,
    Video,
    Unknown,
}

/// The attachment capability selected by its producing adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttachmentKind {
    Image,
    Audio,
    Video,
    Document,
}

/// An explicit media reference. Its target is a path, URL, or data URI.
/// References are carried as fields and never encoded in message text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentReference {
    pub target: String,
    pub kind: AttachmentKind,
}

/// A native geographic pin supplied by a delivery tool, never inferred from text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocationAttachment {
    pub lat: f64,
    pub lng: f64,
    pub name: Option<String>,
    pub address: Option<String>,
}

impl LocationAttachment {
    pub fn is_valid(&self) -> bool {
        (-90.0..=90.0).contains(&self.lat) && (-180.0..=180.0).contains(&self.lng)
    }
}

/// Bytes received from a transport, with an optional durable reference supplied
/// by that transport. The reference is authoritative for attachment disposition.
#[derive(Debug, Clone, Default)]
pub struct MediaAttachment {
    pub file_name: String,
    pub data: Vec<u8>,
    pub mime_type: Option<String>,
    pub reference: Option<AttachmentReference>,
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
        let mut attachment = Self {
            file_name,
            data,
            mime_type,
            reference: None,
        };
        let kind = match attachment.kind() {
            MediaKind::Image => AttachmentKind::Image,
            MediaKind::Audio => AttachmentKind::Audio,
            MediaKind::Video => AttachmentKind::Video,
            MediaKind::Unknown => AttachmentKind::Document,
        };
        attachment.reference = Some(AttachmentReference {
            target: p.display().to_string(),
            kind,
        });
        Ok(attachment)
    }

    pub fn reference_target(&self) -> Option<&str> {
        self.reference
            .as_ref()
            .map(|reference| reference.target.as_str())
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

    /// The MIME the multimodal image loader will assign these bytes, if that
    /// MIME is one the provider path can actually send.
    ///
    /// Where [`looks_like_image`](Self::looks_like_image) answers "must this
    /// turn be treated as carrying an image?" (deliberately permissive,
    /// because its consumers only ever *remove* capability), this answers the
    /// narrower question "will an image reference to these bytes
    /// survive provider preparation?" — so it is the signal for any decision
    /// that *grants* image handling: emitting an image reference, or routing a
    /// turn to a vision provider.
    ///
    /// `None` means the loader would reject the reference and drop it in
    /// favour of a "could not be loaded" note. Callers must then keep the
    /// bytes reachable some other way rather than declaring an image reference
    /// that is guaranteed to be discarded.
    pub fn provider_loadable_image_mime(&self) -> Option<&'static str> {
        provider_loadable_image_mime_for(&self.file_name, &self.data)
    }
}

pub fn provider_loadable_image_mime_for(file_name: &str, data: &[u8]) -> Option<&'static str> {
    let ext = file_name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();

    // Mirrors the loader's own precedence: a declared extension wins over
    // the payload, so bytes that merely *look* loadable cannot rescue a
    // file whose name commits it to a format the provider rejects.
    let resolved = image_mime_from_extension(&ext).or_else(|| image_mime_from_magic(data));

    resolved.filter(|mime| is_provider_image_mime(mime))
}

/// Image MIME types the multimodal provider path accepts.
///
/// Canonical for the whole workspace: the provider's own validation resolves
/// against this list, and channels consult it before promising the loader an
/// image it cannot send.
pub const PROVIDER_IMAGE_MIME_TYPES: &[&str] =
    &["image/png", "image/jpeg", "image/webp", "image/gif"];

/// Whether `mime` is one the multimodal provider path accepts.
pub fn is_provider_image_mime(mime: &str) -> bool {
    PROVIDER_IMAGE_MIME_TYPES.contains(&mime)
}

/// Map a bare file extension to the image MIME the multimodal loader assigns
/// it. Returns `Some` for formats the loader *recognizes*, which is a wider
/// set than it accepts — `bmp` resolves here so the loader can reject it by
/// name instead of failing as an unknown format.
pub fn image_mime_from_extension(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

/// Map leading magic bytes to the image MIME the multimodal loader assigns
/// them. Recognizes the same wider-than-accepted set as
/// [`image_mime_from_extension`].
pub fn image_mime_from_magic(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n']) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(b"BM") {
        return Some("image/bmp");
    }
    None
}

/// Shared image-extension list used by both [`MediaAttachment::kind`] and
/// [`MediaAttachment::looks_like_image`], so the two classifiers cannot
/// drift apart. Extends the loader-recognized set with formats a sender can
/// plausibly call an image even though the provider path cannot send them.
fn is_image_extension(ext: &str) -> bool {
    image_mime_from_extension(ext).is_some() || matches!(ext, "heic" | "tiff" | "svg")
}

/// Detect common image formats from leading magic bytes. Extends the
/// loader-recognized set the same way [`is_image_extension`] does.
fn sniff_image_magic(data: &[u8]) -> bool {
    if image_mime_from_magic(data).is_some() {
        return true;
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
            reference: None,
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
            reference: None,
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
    fn provider_loadable_image_mime_admits_only_what_the_loader_sends() {
        // Extension resolves the format the loader will assume.
        assert_eq!(
            att("photo.png", None).provider_loadable_image_mime(),
            Some("image/png")
        );
        assert_eq!(
            att("photo.JPEG", None).provider_loadable_image_mime(),
            Some("image/jpeg")
        );
        assert_eq!(
            att("sticker.webp", None).provider_loadable_image_mime(),
            Some("image/webp")
        );
        assert_eq!(
            att("anim.gif", None).provider_loadable_image_mime(),
            Some("image/gif")
        );

        // No extension: magic bytes decide, so an image sent "as file" is
        // still loadable.
        assert_eq!(
            att_with_data("upload", None, &[0xFF, 0xD8, 0xFF, 0xE0]).provider_loadable_image_mime(),
            Some("image/jpeg")
        );
    }

    #[test]
    fn provider_loadable_image_mime_rejects_formats_the_loader_drops() {
        // These are images by every conservative signal, but the multimodal
        // loader cannot normalize them: promising an image reference would lose both
        // the bytes and the path.
        for name in ["photo.heic", "scan.tiff", "logo.svg", "old.bmp"] {
            let a = att(name, None);
            assert!(
                a.looks_like_image(),
                "{name} must still count as an image for restriction purposes"
            );
            assert_eq!(
                a.provider_loadable_image_mime(),
                None,
                "{name} must not earn an image reference"
            );
        }

        // TIFF and HEIC magic bytes are recognized as images but are equally
        // unloadable, so an extensionless upload of one is not marked either.
        assert!(att_with_data("upload", None, &[0x49, 0x49, 0x2A, 0x00]).looks_like_image());
        assert_eq!(
            att_with_data("upload", None, &[0x49, 0x49, 0x2A, 0x00]).provider_loadable_image_mime(),
            None
        );
    }

    #[test]
    fn provider_loadable_image_mime_lets_the_declared_extension_win() {
        // The loader resolves the extension before sniffing, so a name that
        // commits the file to an unsupported format is rejected even when the
        // payload would have sniffed as a supported one. Matching that
        // precedence here is what keeps the attachment decision honest.
        let a = att_with_data("photo.bmp", None, &[0xFF, 0xD8, 0xFF, 0xE0]);
        assert_eq!(a.provider_loadable_image_mime(), None);

        // A sender-declared MIME does not widen the set either; only the
        // loader's own resolution counts.
        let b = att("scan.tiff", Some("image/png"));
        assert_eq!(b.provider_loadable_image_mime(), None);
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
