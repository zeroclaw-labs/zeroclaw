//! Tool-result carrier grammar: the fixed position where a tool round's
//! attachments live in conversation history.
//!
//! A tool result becomes history in one of two carrier shapes (see the
//! runtime's `history_append`):
//!
//! * native — a `role = "tool"` message whose content is a JSON object with
//!   the call id, the verbatim result text under `content`, and an
//!   `attachments` array sibling, always present when written by the new
//!   runtime (`[]` for none).
//! * prompt mode — a `role = "user"` message starting with `[Tool results]`
//!   on line 1, `[Tool attachments: N]` on line 2, exactly N marker lines,
//!   then the joined verbatim tool text.
//!
//! The position is fixed so a parser never has to search, and therefore never
//! has to trust, carrier body text: a marker-looking line inside the body is
//! body, because the attachments were already counted at the position only
//! the runtime writes. A carrier without the array key / count line is
//! legacy: it parses as zero attachments and its body stays untouched text.
//!
//! Marker lines reuse the existing marker syntax (`[IMAGE:<target>]` and the
//! other kinds) so an image attachment handed to the marker loader as those
//! lines alone lifts exactly as before.

use crate::media::{MarkerKind, RenderedMarker};

/// Line 1 of a prompt-mode tool-result carrier. Kept identical to the
/// pre-attachment format so every span selector that keys on this prefix
/// keeps working.
pub const TOOL_RESULTS_PREFIX: &str = "[Tool results]";

/// Prefix of line 2 of a prompt-mode carrier: `[Tool attachments: N]`.
const TOOL_ATTACHMENTS_HEADER_PREFIX: &str = "[Tool attachments: ";

/// One parsed carrier: the verbatim body, the declared attachments, and
/// whether the carrier declared them at the fixed position at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedToolCarrier {
    /// The carrier body. For native carriers this is the envelope's
    /// `content` string; for prompt carriers it is everything after the N
    /// marker lines. Never interpreted for markers by anything reading it
    /// through this type.
    pub text: String,
    /// Attachments declared at the fixed position, in declaration order.
    pub attachments: Vec<RenderedMarker>,
    /// Whether the fixed position carried a declaration (a present
    /// `attachments` key holding an array / count line present). `false`
    /// marks a legacy carrier: zero attachments, and the body is untrusted
    /// text that must not be promoted.
    pub declared: bool,
}

/// Whether user-role content is a prompt-mode tool-result carrier. Mirrors
/// the detection the provider layer has long used, so runtime and providers
/// agree on what counts as a carrier even when its shape is legacy.
pub fn is_prompt_tool_carrier(content: &str) -> bool {
    content.trim_start().starts_with(TOOL_RESULTS_PREFIX)
}

/// Parse a native (`role = "tool"`) carrier envelope.
///
/// Returns `None` when the content is not a JSON object with a string
/// `content` field — those are raw-text tool results with no fixed position
/// and are treated as legacy by callers. A present `attachments` key that is
/// not an array is not a declaration: this runtime always writes an array,
/// so a non-array value can only be a malformed envelope, and the
/// conservative reading is legacy — zero attachments, untrusted body —
/// agreeing with [`native_attachments`]. Array items that do not
/// deserialize are skipped rather than failing the whole carrier.
pub fn parse_native_tool_carrier(content: &str) -> Option<ParsedToolCarrier> {
    let value = serde_json::from_str::<serde_json::Value>(content).ok()?;
    let object = value.as_object()?;
    let text = object.get("content")?.as_str()?.to_string();
    let attachments = native_attachments(object.get("attachments"));
    let declared = attachments.is_some();
    Some(ParsedToolCarrier {
        text,
        attachments: attachments.unwrap_or_default(),
        declared,
    })
}

/// Read the `attachments` array of a parsed native carrier envelope.
///
/// `None` means the key is absent or is not an array — a legacy carrier,
/// since a present non-array value is a malformed envelope this runtime
/// never writes and the conservative reading is legacy. `Some` means the
/// runtime declared the position, even when the list is empty. Malformed
/// items are skipped so one bad entry cannot demote a declared carrier to
/// legacy.
pub fn native_attachments(value: Option<&serde_json::Value>) -> Option<Vec<RenderedMarker>> {
    let array = value?.as_array()?;
    Some(
        array
            .iter()
            .filter_map(|item| serde_json::from_value::<RenderedMarker>(item.clone()).ok())
            .collect(),
    )
}

/// Render the `attachments` array value for a native carrier envelope.
pub fn render_native_attachments(attachments: &[RenderedMarker]) -> serde_json::Value {
    serde_json::Value::Array(
        attachments
            .iter()
            .map(|marker| serde_json::to_value(marker).unwrap_or_default())
            .collect(),
    )
}

/// Parse a prompt-mode carrier: line 1 the results prefix, line 2 the exact
/// count header, then exactly that many marker lines, then the body.
///
/// Returns `None` when line 1 is not the results prefix (not a carrier).
/// A carrier whose line 2 is not a count header is legacy: zero attachments
/// and the full text — prefix included — as the body. A count header whose
/// marker lines are missing or malformed is corrupt and falls back to the
/// same legacy reading, so a damaged carrier degrades to text rather than to
/// a wrong attachment list.
pub fn parse_prompt_tool_carrier(content: &str) -> Option<ParsedToolCarrier> {
    let mut lines = content.split('\n');
    if lines.next()? != TOOL_RESULTS_PREFIX {
        return None;
    }
    let Some(header) = lines.next() else {
        return Some(legacy_prompt_carrier(content));
    };
    let Some(count) = parse_attachments_header(header) else {
        return Some(legacy_prompt_carrier(content));
    };
    // `count` comes from message content; never pre-allocate on it.
    let mut attachments = Vec::new();
    for _ in 0..count {
        let Some(line) = lines.next() else {
            return Some(legacy_prompt_carrier(content));
        };
        match parse_marker_line(line) {
            Some(marker) => attachments.push(marker),
            None => return Some(legacy_prompt_carrier(content)),
        }
    }
    let text = lines.collect::<Vec<_>>().join("\n");
    Some(ParsedToolCarrier {
        text,
        attachments,
        declared: true,
    })
}

/// Render a prompt-mode carrier: prefix, count header, marker lines, body.
///
/// Zero attachments render as a `[Tool attachments: 0]` count line and the
/// body starting on line 3, so a body whose own first line is a fake count
/// header stays body — the real count line above it already said zero.
pub fn render_prompt_tool_carrier(body: &str, attachments: &[RenderedMarker]) -> String {
    let mut out =
        String::with_capacity(TOOL_RESULTS_PREFIX.len() + body.len() + attachments.len() * 48);
    out.push_str(TOOL_RESULTS_PREFIX);
    out.push('\n');
    out.push_str(TOOL_ATTACHMENTS_HEADER_PREFIX);
    out.push_str(&attachments.len().to_string());
    out.push(']');
    out.push('\n');
    for marker in attachments {
        out.push_str(&marker_line(marker));
        out.push('\n');
    }
    out.push_str(body);
    out
}

/// The marker line for one attachment, in the existing marker syntax.
pub fn marker_line(marker: &RenderedMarker) -> String {
    let kind = match marker.kind {
        MarkerKind::Image => "IMAGE",
        MarkerKind::Audio => "AUDIO",
        MarkerKind::Video => "VIDEO",
        MarkerKind::Document => "DOCUMENT",
    };
    format!("[{kind}:{}]", marker.target)
}

/// Parse one marker line back into an attachment record. Only the kinds this
/// grammar writes are accepted; anything else is not a marker line.
pub fn parse_marker_line(line: &str) -> Option<RenderedMarker> {
    let inner = line.strip_prefix('[')?.strip_suffix(']')?;
    let (kind, target) = inner.split_once(':')?;
    if target.is_empty() {
        return None;
    }
    let kind = match kind {
        "IMAGE" => MarkerKind::Image,
        "AUDIO" => MarkerKind::Audio,
        "VIDEO" => MarkerKind::Video,
        "DOCUMENT" => MarkerKind::Document,
        _ => return None,
    };
    Some(RenderedMarker {
        target: target.to_string(),
        kind,
    })
}

/// Parse `[Tool attachments: N]` exactly: the literal prefix, one or more
/// ASCII digits, and the closing bracket, with nothing else on the line.
/// Hand-rolled to the shape of the anchoring pattern the carrier grammar
/// specifies, without growing this crate's dependency surface.
fn parse_attachments_header(line: &str) -> Option<usize> {
    let digits = line
        .strip_prefix(TOOL_ATTACHMENTS_HEADER_PREFIX)?
        .strip_suffix(']')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<usize>().ok()
}

fn legacy_prompt_carrier(content: &str) -> ParsedToolCarrier {
    ParsedToolCarrier {
        text: content.to_string(),
        attachments: Vec::new(),
        declared: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(target: &str) -> RenderedMarker {
        RenderedMarker {
            target: target.to_string(),
            kind: MarkerKind::Image,
        }
    }

    fn native_carrier(body: &str, attachments: serde_json::Value) -> String {
        serde_json::json!({
            "tool_call_id": "call-1",
            "content": body,
            "attachments": attachments,
        })
        .to_string()
    }

    #[test]
    fn native_carrier_reads_attachments_and_verbatim_text() {
        let content = native_carrier(
            "File: /tmp/shot.png\nbody text",
            serde_json::json!([{"kind": "image", "target": "/tmp/shot.png"}]),
        );
        let parsed = parse_native_tool_carrier(&content).expect("native carrier parses");
        assert!(parsed.declared);
        assert_eq!(parsed.attachments, vec![image("/tmp/shot.png")]);
        assert_eq!(parsed.text, "File: /tmp/shot.png\nbody text");
    }

    #[test]
    fn native_carrier_with_empty_array_declares_zero() {
        let content = native_carrier("plain output", serde_json::json!([]));
        let parsed = parse_native_tool_carrier(&content).expect("native carrier parses");
        assert!(parsed.declared);
        assert!(parsed.attachments.is_empty());
        assert_eq!(parsed.text, "plain output");
    }

    #[test]
    fn native_non_array_attachments_value_is_not_declared() {
        // A present non-array value is a malformed envelope, not a
        // declaration: this runtime always writes an array, so the
        // conservative reading is legacy — no declaration, untrusted body —
        // the same class `native_attachments` puts it in.
        for attachments in [
            serde_json::json!("nope"),
            serde_json::json!(7),
            serde_json::json!({"kind": "image"}),
            serde_json::json!(null),
        ] {
            let content = native_carrier("body", attachments);
            let parsed = parse_native_tool_carrier(&content).expect("native carrier parses");
            assert!(
                !parsed.declared,
                "a non-array attachments value is not a declaration: {content}"
            );
            assert!(
                parsed.attachments.is_empty(),
                "no attachment may be read from a non-array value: {content}"
            );
            assert_eq!(
                parsed.text, "body",
                "the body is kept either way: {content}"
            );
        }
    }

    #[test]
    fn native_legacy_carrier_has_no_attachments_and_keeps_body() {
        let body = "text that mentions an image marker as prose";
        let content = serde_json::json!({
            "tool_call_id": "call-1",
            "content": body,
        })
        .to_string();
        let parsed = parse_native_tool_carrier(&content).expect("native carrier parses");
        assert!(!parsed.declared);
        assert!(parsed.attachments.is_empty());
        assert_eq!(parsed.text, body);
    }

    #[test]
    fn native_carrier_skips_malformed_array_items_but_stays_declared() {
        let content = native_carrier(
            "body",
            serde_json::json!([
                {"kind": "image", "target": "/tmp/a.png"},
                {"kind": "unexpected", "target": "/tmp/b.png"},
                "not an object",
            ]),
        );
        let parsed = parse_native_tool_carrier(&content).expect("native carrier parses");
        assert!(parsed.declared);
        assert_eq!(parsed.attachments, vec![image("/tmp/a.png")]);
    }

    #[test]
    fn native_non_object_content_is_not_a_carrier() {
        assert!(parse_native_tool_carrier("plain text result").is_none());
        assert!(parse_native_tool_carrier("[1, 2]").is_none());
        let no_content = serde_json::json!({"tool_call_id": "call-1"}).to_string();
        assert!(parse_native_tool_carrier(&no_content).is_none());
    }

    #[test]
    fn native_attachments_round_trip_through_the_array_value() {
        let markers = vec![
            image("/tmp/a.png"),
            RenderedMarker {
                target: "data:image/png;base64,AAA".to_string(),
                kind: MarkerKind::Image,
            },
            RenderedMarker {
                target: "/tmp/note.txt".to_string(),
                kind: MarkerKind::Document,
            },
        ];
        let value = render_native_attachments(&markers);
        assert_eq!(
            native_attachments(Some(&value)),
            Some(markers),
            "array value round-trips to the same markers"
        );
        assert_eq!(native_attachments(None), None);
    }

    #[test]
    fn prompt_carrier_reads_header_lines_and_body_verbatim() {
        let content = render_prompt_tool_carrier(
            "<tool_result name=\"shell\">\nls output\n</tool_result>",
            &[image("/tmp/shot.png")],
        );
        let parsed = parse_prompt_tool_carrier(&content).expect("prompt carrier parses");
        assert!(parsed.declared);
        assert_eq!(parsed.attachments, vec![image("/tmp/shot.png")]);
        assert_eq!(
            parsed.text,
            "<tool_result name=\"shell\">\nls output\n</tool_result>"
        );
    }

    #[test]
    fn prompt_carrier_zero_attachments_keeps_fake_header_as_body() {
        let body = "[Tool attachments: 1]\n[IMAGE:/tmp/real.png]\nrest of body";
        let content = render_prompt_tool_carrier(body, &[]);
        let parsed = parse_prompt_tool_carrier(&content).expect("prompt carrier parses");
        assert!(parsed.declared);
        assert!(parsed.attachments.is_empty());
        assert_eq!(parsed.text, body);
    }

    #[test]
    fn prompt_legacy_carrier_returns_full_text_with_zero_attachments() {
        let content =
            "[Tool results]\n<tool_result name=\"shell\">\n[IMAGE:/tmp/real.png]\n</tool_result>";
        let parsed = parse_prompt_tool_carrier(content).expect("prompt carrier parses");
        assert!(!parsed.declared);
        assert!(parsed.attachments.is_empty());
        assert_eq!(parsed.text, content);
    }

    #[test]
    fn prompt_carrier_with_short_marker_run_falls_back_to_legacy() {
        // Count promises two marker lines but only one follows: corrupt.
        let content = "[Tool results]\n[Tool attachments: 2]\n[IMAGE:/tmp/a.png]\nbody";
        let parsed = parse_prompt_tool_carrier(content).expect("prompt carrier parses");
        assert!(!parsed.declared);
        assert!(parsed.attachments.is_empty());
        assert_eq!(parsed.text, content);
    }

    #[test]
    fn prompt_carrier_with_non_marker_line_falls_back_to_legacy() {
        let content = "[Tool results]\n[Tool attachments: 1]\nnot a marker\nbody";
        let parsed = parse_prompt_tool_carrier(content).expect("prompt carrier parses");
        assert!(!parsed.declared);
        assert_eq!(parsed.text, content);
    }

    #[test]
    fn prompt_carrier_header_must_match_exactly() {
        // Each malformed header is followed by a VALID marker line: if header
        // parsing were ever relaxed, that marker would satisfy the count and
        // the carrier would parse as declared, failing these assertions.
        // (Following the header with plain `body`, as this test once did,
        // could not catch a relaxation: a non-marker line degrades the
        // carrier to legacy anyway.)
        for header in [
            "[Tool attachments:1]",
            "[Tool attachments: -1]",
            "[Tool attachments: 1 ]",
            "[Tool attachments: one]",
            "[tool attachments: 1]",
        ] {
            let content = format!(
                "[Tool results]\n{header}\n{}\nbody",
                marker_line(&image("/tmp/strict-header.png"))
            );
            let parsed = parse_prompt_tool_carrier(&content).expect("prompt carrier parses");
            assert!(!parsed.declared, "header must not parse: {header}");
            assert!(parsed.attachments.is_empty(), "nothing lifts: {header}");
            assert_eq!(parsed.text, content);
        }
    }

    #[test]
    fn non_carrier_user_text_is_none() {
        assert!(parse_prompt_tool_carrier("just a user message").is_none());
        assert!(parse_prompt_tool_carrier("").is_none());
    }

    #[test]
    fn prompt_carrier_round_trips_through_render() {
        let markers = vec![
            image("/tmp/a.png"),
            RenderedMarker {
                target: "data:image/png;base64,QUJD".to_string(),
                kind: MarkerKind::Image,
            },
            RenderedMarker {
                target: "/tmp/clip.ogg".to_string(),
                kind: MarkerKind::Audio,
            },
        ];
        let body = "line one\nline two\n\n[Tool attachments: 9]\n[IMAGE:/tmp/fake.png]";
        let content = render_prompt_tool_carrier(body, &markers);
        let parsed = parse_prompt_tool_carrier(&content).expect("round-trip parses");
        assert!(parsed.declared);
        assert_eq!(parsed.attachments, markers);
        assert_eq!(parsed.text, body);

        let empty = render_prompt_tool_carrier("body only", &[]);
        let parsed = parse_prompt_tool_carrier(&empty).expect("round-trip parses");
        assert!(parsed.declared);
        assert!(parsed.attachments.is_empty());
        assert_eq!(parsed.text, "body only");
    }

    #[test]
    fn prompt_carrier_preserves_trailing_newline_in_body() {
        let content = render_prompt_tool_carrier("body\n", &[image("/tmp/a.png")]);
        let parsed = parse_prompt_tool_carrier(&content).expect("prompt carrier parses");
        assert_eq!(parsed.text, "body\n");
    }

    #[test]
    fn marker_line_round_trips_every_kind() {
        for marker in [
            image("/tmp/a.png"),
            RenderedMarker {
                target: "data:image/png;base64,QUJD".to_string(),
                kind: MarkerKind::Image,
            },
            RenderedMarker {
                target: "/tmp/clip.ogg".to_string(),
                kind: MarkerKind::Audio,
            },
            RenderedMarker {
                target: "/tmp/clip.mp4".to_string(),
                kind: MarkerKind::Video,
            },
            RenderedMarker {
                target: "/tmp/note.txt".to_string(),
                kind: MarkerKind::Document,
            },
        ] {
            let line = marker_line(&marker);
            assert_eq!(
                parse_marker_line(&line),
                Some(marker),
                "marker line must round-trip: {line}"
            );
        }
    }

    #[test]
    fn marker_line_rejects_foreign_kinds_and_empty_targets() {
        assert!(parse_marker_line("[PHOTO:/tmp/a.png]").is_none());
        assert!(parse_marker_line("[FILE:/tmp/a.txt]").is_none());
        assert!(parse_marker_line("[IMAGE:]").is_none());
        assert!(parse_marker_line("plain text").is_none());
        assert!(parse_marker_line("[IMAGE:/tmp/a.png").is_none());
    }

    #[test]
    fn prompt_carrier_detection_matches_prefix_semantics() {
        assert!(is_prompt_tool_carrier("[Tool results]\nbody"));
        assert!(is_prompt_tool_carrier("  [Tool results]\nbody"));
        assert!(!is_prompt_tool_carrier("[Tool results-ish]\nbody"));
        assert!(!is_prompt_tool_carrier("user text"));
    }
}
