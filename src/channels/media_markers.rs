//! Shared parser for outgoing media markers in channel messages.
//!
//! Parses `[KIND:target]` markers from message content where KIND is one of:
//! IMAGE|PHOTO, DOCUMENT|FILE, VIDEO, AUDIO, VOICE.
//!
//! Used by Lark and other channels to extract media attachments for upload/send.

/// Media kind for outgoing attachments (parsed from marker prefix).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutgoingMediaKind {
    Image,
    Document,
    Video,
    Audio,
    Voice,
}

/// A single media part parsed from a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingMediaPart {
    pub kind: OutgoingMediaKind,
    /// Raw target string (path or URL). Channels decide how to resolve it.
    pub target: String,
}

impl OutgoingMediaKind {
    /// Parse marker prefix (e.g. "IMAGE", "DOCUMENT") case-insensitively.
    pub fn from_marker_kind(kind: &str) -> Option<Self> {
        match kind.trim().to_ascii_uppercase().as_str() {
            "IMAGE" | "PHOTO" => Some(Self::Image),
            "DOCUMENT" | "FILE" => Some(Self::Document),
            "VIDEO" => Some(Self::Video),
            "AUDIO" => Some(Self::Audio),
            "VOICE" => Some(Self::Voice),
            _ => None,
        }
    }
}

fn find_matching_close(s: &str) -> Option<usize> {
    let mut depth = 1usize;
    for (i, ch) in s.char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse `[KIND:target]` markers from message content.
///
/// Returns `(cleaned_text, parts)` where:
/// - `cleaned_text`: message with recognized markers removed
/// - `parts`: parsed media parts in order of appearance
///
/// Unknown or invalid markers are preserved in the text.
pub fn parse_outgoing_media_markers(message: &str) -> (String, Vec<OutgoingMediaPart>) {
    let mut cleaned = String::with_capacity(message.len());
    let mut parts = Vec::new();
    let mut cursor = 0usize;

    while cursor < message.len() {
        let Some(open_rel) = message[cursor..].find('[') else {
            cleaned.push_str(&message[cursor..]);
            break;
        };

        let open = cursor + open_rel;
        cleaned.push_str(&message[cursor..open]);

        let Some(close_rel) = find_matching_close(&message[open + 1..]) else {
            cleaned.push_str(&message[open..]);
            break;
        };

        let close = open + 1 + close_rel;
        let marker_text = &message[open + 1..close];

        let parsed = marker_text.split_once(':').and_then(|(kind, target)| {
            let kind = OutgoingMediaKind::from_marker_kind(kind)?;
            let target = target.trim();
            if target.is_empty() {
                return None;
            }
            Some(OutgoingMediaPart {
                kind,
                target: target.to_string(),
            })
        });

        if let Some(part) = parsed {
            parts.push(part);
        } else {
            // Unknown / invalid marker: preserve original text.
            cleaned.push_str(&message[open..=close]);
        }

        cursor = close + 1;
    }

    (cleaned.trim().to_string(), parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_extracts_image_marker() {
        let (cleaned, parts) = parse_outgoing_media_markers("See [IMAGE:/tmp/a.png] here");
        assert_eq!(cleaned, "See  here");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].kind, OutgoingMediaKind::Image);
        assert_eq!(parts[0].target, "/tmp/a.png");
    }

    #[test]
    fn parse_extracts_multiple_types() {
        let input = "Report [IMAGE:/a.png] and [DOCUMENT:/b.pdf]";
        let (cleaned, parts) = parse_outgoing_media_markers(input);
        assert_eq!(cleaned, "Report  and ");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].kind, OutgoingMediaKind::Image);
        assert_eq!(parts[0].target, "/a.png");
        assert_eq!(parts[1].kind, OutgoingMediaKind::Document);
        assert_eq!(parts[1].target, "/b.pdf");
    }

    #[test]
    fn parse_preserves_invalid_markers() {
        let input = "Hello [NOT_A_MARKER:foo] world";
        let (cleaned, parts) = parse_outgoing_media_markers(input);
        assert_eq!(cleaned, input);
        assert!(parts.is_empty());
    }

    #[test]
    fn parse_empty_target_keeps_marker() {
        let input = "x [IMAGE: ] y";
        let (cleaned, parts) = parse_outgoing_media_markers(input);
        assert_eq!(cleaned, input);
        assert!(parts.is_empty());
    }

    #[test]
    fn parse_photo_alias() {
        let (_, parts) = parse_outgoing_media_markers("[PHOTO:/p.jpg]");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].kind, OutgoingMediaKind::Image);
    }
}
