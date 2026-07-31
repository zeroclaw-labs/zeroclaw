/// Truncate a string to `max_chars` Unicode characters, appending "..." if truncated.
pub fn truncate_with_ellipsis(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => {
            let truncated = &s[..idx];
            format!("{}...", truncated.trim_end())
        }
        None => s.to_string(),
    }
}

/// Largest byte index `<= max_bytes` that is still a valid UTF-8 boundary.
pub fn floor_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

pub const BLOCK_KIT_PREFIX: &str = "__ZEROCLAW_BLOCK_KIT__";

pub fn strip_tool_call_tags(message: &str) -> String {
    const TOOL_CALL_OPEN_TAGS: [&str; 7] = [
        "<function_calls>",
        "<function_call>",
        "<tool_call>",
        "<toolcall>",
        "<tool-call>",
        "<tool>",
        "<invoke>",
    ];

    fn find_first_tag<'a>(haystack: &str, tags: &'a [&'a str]) -> Option<(usize, &'a str)> {
        tags.iter()
            .filter_map(|tag| haystack.find(tag).map(|idx| (idx, *tag)))
            .min_by_key(|(idx, _)| *idx)
    }

    fn matching_close_tag(open_tag: &str) -> Option<&'static str> {
        match open_tag {
            "<function_calls>" => Some("</function_calls>"),
            "<function_call>" => Some("</function_call>"),
            "<tool_call>" => Some("</tool_call>"),
            "<toolcall>" => Some("</toolcall>"),
            "<tool-call>" => Some("</tool-call>"),
            "<tool>" => Some("</tool>"),
            "<invoke>" => Some("</invoke>"),
            _ => None,
        }
    }

    fn extract_first_json_end(input: &str) -> Option<usize> {
        let trimmed = input.trim_start();
        let trim_offset = input.len().saturating_sub(trimmed.len());

        for (byte_idx, ch) in trimmed.char_indices() {
            if ch != '{' && ch != '[' {
                continue;
            }

            let slice = &trimmed[byte_idx..];
            let mut stream =
                serde_json::Deserializer::from_str(slice).into_iter::<serde_json::Value>();
            if let Some(Ok(_value)) = stream.next() {
                let consumed = stream.byte_offset();
                if consumed > 0 {
                    return Some(trim_offset + byte_idx + consumed);
                }
            }
        }

        None
    }

    fn strip_leading_close_tags(mut input: &str) -> &str {
        loop {
            let trimmed = input.trim_start();
            if !trimmed.starts_with("</") {
                return trimmed;
            }

            let Some(close_end) = trimmed.find('>') else {
                return "";
            };
            input = &trimmed[close_end + 1..];
        }
    }

    fn tool_structure_runs_to_end(inner: &str) -> bool {
        let mut rest = inner.trim_start();
        // Consume a run of `<...>` tags (and whitespace between them).
        while rest.starts_with('<') {
            match rest.find('>') {
                Some(gt) => rest = rest[gt + 1..].trim_start(),
                // Cut off mid-tag (no closing '>') — a classic truncation.
                None => return true,
            }
        }
        let tail = rest.trim();
        if tail.is_empty() {
            // Tags ran cleanly to the end → truncation.
            return true;
        }
        // Non-empty tail: prose ⇒ inline example (keep); otherwise it's a
        // truncated tag/param value (drop).
        !looks_like_prose(tail)
    }

    // Heuristic: does `text` read like resumed natural-language prose (as opposed
    // to a cut-off parameter value)? True on an internal sentence boundary
    // (". " / "! " / "? " + a letter) or a multi-word string that ends like a
    // sentence. Deliberately lenient so ambiguous tails are kept, not dropped.
    fn looks_like_prose(text: &str) -> bool {
        let bytes = text.as_bytes();
        for i in 0..bytes.len().saturating_sub(1) {
            if matches!(bytes[i], b'.' | b'!' | b'?')
                && matches!(bytes[i + 1], b' ' | b'\n' | b'\t')
                && text[i + 1..]
                    .trim_start()
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic())
            {
                return true;
            }
        }
        let trimmed = text.trim_end();
        let ends_like_sentence = trimmed
            .chars()
            .last()
            .is_some_and(|c| matches!(c, '.' | '!' | '?'))
            && trimmed
                .chars()
                .rev()
                .nth(1)
                .is_some_and(|c| c.is_alphabetic());
        ends_like_sentence && text.trim().contains(' ')
    }

    let mut kept_segments = Vec::new();
    let mut remaining = message;

    while let Some((start, open_tag)) = find_first_tag(remaining, &TOOL_CALL_OPEN_TAGS) {
        let before = &remaining[..start];
        if !before.is_empty() {
            kept_segments.push(before.to_string());
        }

        let Some(close_tag) = matching_close_tag(open_tag) else {
            break;
        };
        let after_open = &remaining[start + open_tag.len()..];

        if let Some(close_idx) = after_open.find(close_tag) {
            remaining = &after_open[close_idx + close_tag.len()..];
            continue;
        }

        if let Some(consumed_end) = extract_first_json_end(after_open) {
            remaining = strip_leading_close_tags(&after_open[consumed_end..]);
            continue;
        }

        let inner = after_open.trim_start();
        let inner_lower = inner.to_ascii_lowercase();
        let looks_like_tool_structure = inner_lower.starts_with("<invoke")
            || inner_lower.starts_with("<parameter")
            || inner_lower.starts_with("<tool")
            || inner_lower.starts_with("<function")
            || inner.starts_with('{')
            || inner.starts_with('[');
        if looks_like_tool_structure && tool_structure_runs_to_end(inner) {
            remaining = "";
            break;
        }

        kept_segments.push(remaining[start..].to_string());
        remaining = "";
        break;
    }

    if !remaining.is_empty() {
        kept_segments.push(remaining.to_string());
    }

    let mut result = kept_segments.concat();

    // Clean up any resulting blank lines (but preserve paragraphs)
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }

    result.trim().to_string()
}

/// Recognized attachment marker kinds (e.g. `[IMAGE:/path]`, `[DOCUMENT:url]`).
const ATTACHMENT_KINDS: &[&str] = &[
    "IMAGE", "PHOTO", "DOCUMENT", "FILE", "VIDEO", "AUDIO", "VOICE", "LOCATION",
];

/// Parse `[KIND:target]` attachment markers out of a message.
/// Returns cleaned text (markers removed) and a vec of `(kind, target)` pairs.
pub fn parse_attachment_markers(message: &str) -> (String, Vec<(String, String)>) {
    parse_attachment_markers_of_kinds(message, ATTACHMENT_KINDS)
}

pub(crate) fn parse_attachment_markers_of_kinds(
    message: &str,
    kinds: &[&str],
) -> (String, Vec<(String, String)>) {
    let mut cleaned = String::with_capacity(message.len());
    let mut attachments = Vec::new();
    let mut cursor = 0usize;

    while let Some(rel_start) = message[cursor..].find('[') {
        let start = cursor + rel_start;
        cleaned.push_str(&message[cursor..start]);

        let Some(rel_end) = message[start..].find(']') else {
            cleaned.push_str(&message[start..]);
            cursor = message.len();
            break;
        };
        let end = start + rel_end;
        let marker_text = &message[start + 1..end];

        let parsed = marker_text.split_once(':').and_then(|(kind, target)| {
            let kind_upper = kind.trim().to_ascii_uppercase();
            let target = target.trim();
            if target.is_empty() || !kinds.contains(&kind_upper.as_str()) {
                return None;
            }
            Some((kind_upper, target.to_string()))
        });

        if let Some(attachment) = parsed {
            attachments.push(attachment);
        } else {
            cleaned.push_str(&message[start..=end]);
        }

        cursor = end + 1;
    }

    if cursor < message.len() {
        cleaned.push_str(&message[cursor..]);
    }

    (cleaned.trim().to_string(), attachments)
}

/// A native location pin parsed from a `[LOCATION:...]` marker. Shared by
/// both WhatsApp backends (web protobuf send and Cloud API JSON send).
#[cfg(any(feature = "whatsapp-web", feature = "channel-whatsapp-cloud", test))]
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WhatsAppLocation {
    pub(crate) lat: f64,
    pub(crate) lng: f64,
    pub(crate) name: Option<String>,
    pub(crate) address: Option<String>,
}

#[cfg(any(feature = "whatsapp-web", feature = "channel-whatsapp-cloud", test))]
impl WhatsAppLocation {
    pub(crate) fn parse(target: &str) -> Option<Self> {
        // Extract the next field.  If the trimmed input starts with `"` the
        // field runs to the closing `"` and may contain commas; otherwise the
        // field ends at the first `,`.  Returns `(field, rest)` — `rest` has
        // already been trimmed and stripped of the separating comma.
        fn next_field(s: &str) -> (&str, &str) {
            let s = s.trim();
            if let Some(inner) = s.strip_prefix('"') {
                if let Some(end) = inner.find('"') {
                    // Quoted field: grab everything up to the closing quote.
                    let field = &inner[..end];
                    let rest = inner[end + 1..].trim();
                    let rest = match rest.strip_prefix(',') {
                        Some(s) => s.trim(),
                        None => "",
                    };
                    return (field, rest);
                }
                // Unclosed quote — treat the rest as one field.
                return (inner, "");
            }
            // Plain field: split on the first comma.
            match s.find(',') {
                Some(pos) => (s[..pos].trim(), s[pos + 1..].trim()),
                None => (s, ""),
            }
        }

        let (lat_str, rest) = next_field(target);
        let lat: f64 = lat_str.parse().ok()?;
        let (lng_str, rest) = next_field(rest);
        let lng: f64 = lng_str.parse().ok()?;
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lng) {
            return None;
        }

        let (name_raw, rest) = next_field(rest);
        let name = (!name_raw.is_empty()).then(|| name_raw.to_string());

        let address = (!rest.is_empty()).then(|| rest.to_string());

        Some(Self {
            lat,
            lng,
            name,
            address,
        })
    }
}

/// Render an inbound static location as chat text, e.g.
/// `[Location: 40.712800, -74.006000 — NYC]`. Shared by both WhatsApp
/// backends so inbound pins read identically regardless of transport.
#[cfg(any(feature = "whatsapp-web", feature = "channel-whatsapp-cloud", test))]
pub(crate) fn format_location_content(lat: f64, lng: f64, name: Option<&str>) -> String {
    match name.filter(|n| !n.is_empty()) {
        Some(name) => format!("[Location: {lat:.6}, {lng:.6} — {name}]"),
        None => format!("[Location: {lat:.6}, {lng:.6}]"),
    }
}

#[cfg(any(
    feature = "channel-discord",
    feature = "channel-signal",
    feature = "channel-slack",
    feature = "channel-whatsapp-cloud",
    feature = "whatsapp-web",
    test
))]
pub(crate) fn new_approval_token() -> String {
    use rand::RngExt;
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..6)
        .map(|_| CHARSET[rng.random_range(0..CHARSET.len())] as char)
        .collect()
}

pub fn parse_approval_reply(
    text: &str,
) -> Option<(String, zeroclaw_api::channel::ChannelApprovalResponse)> {
    use zeroclaw_api::channel::ChannelApprovalResponse;
    let lower = text.trim().to_lowercase();
    let mut parts = lower.splitn(2, ' ');
    let token = parts.next()?.to_string();
    if token.len() != 6 || !token.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let action_word = parts.next()?.split_whitespace().next()?;
    let response = match action_word {
        "yes" | "y" | "approve" => ChannelApprovalResponse::Approve,
        "no" | "n" | "deny" => ChannelApprovalResponse::Deny,
        "always" => ChannelApprovalResponse::AlwaysApprove,
        _ => return None,
    };
    Some((token, response))
}

/// Generate a conversation history key from a channel message.
pub fn conversation_history_key(msg: &zeroclaw_api::channel::ChannelMessage) -> String {
    match &msg.thread_ts {
        Some(tid) => format!(
            "{}_{}_{}_{}",
            msg.channel, msg.reply_target, tid, msg.sender
        ),
        None => format!("{}_{}_{}", msg.channel, msg.reply_target, msg.sender),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_char_boundary_handles_mid_codepoint_offset() {
        let text = "abc😀def";

        assert_eq!(super::floor_char_boundary(text, 5), 3);
        assert_eq!(super::floor_char_boundary(text, usize::MAX), text.len());
    }

    #[test]
    fn strip_drops_truncated_function_calls_envelope_keeps_prose() {
        // Truncated `<function_calls><invoke …><parameter …` (model cut off):
        // the broken tail is dropped, the preceding prose survives.
        let msg = "Here's the result:\n<function_calls>\n<invoke name=\"shell\">\n<parameter name=\"command\">sed -n '1,5p' file.rs";
        assert_eq!(strip_tool_call_tags(msg), "Here's the result:");

        // Envelope-only (no prose) -> empty.
        let only = "<function_calls>\n<invoke name=\"shell\">\n<parameter name=\"command\">date";
        assert_eq!(strip_tool_call_tags(only), "");

        // Complete envelope is still stripped (unchanged behaviour).
        let complete = "before <function_calls><invoke name=\"shell\"><parameter name=\"command\">date</parameter></invoke></function_calls> after";
        assert_eq!(strip_tool_call_tags(complete), "before  after");
    }

    #[test]
    fn strip_keeps_prose_that_merely_mentions_a_tag() {
        // An unterminated opener followed by ordinary prose (not tool structure)
        // is kept — the model is talking about the tag, not calling a tool.
        let msg =
            "The bug is that models emit <function_calls> and never close it, hanging the parser.";
        assert_eq!(strip_tool_call_tags(msg), msg);
    }

    #[test]
    fn strip_keeps_unterminated_example_followed_by_prose() {
        // An unterminated opener IS followed by tool structure, but prose
        // resumes after it — so it's an inline example, not a truncation.
        // Keep it verbatim (the EOF rule: a real truncation ends the message).
        let xml_example = "The model emits <function_calls><invoke name=\"x\"> and then keeps going. This sentence matters.";
        assert_eq!(strip_tool_call_tags(xml_example), xml_example);

        let json_example = "Emit <tool_call> {then describe the schema} in your docs.";
        assert_eq!(strip_tool_call_tags(json_example), json_example);
    }

    #[test]
    fn strip_still_drops_genuine_truncation_to_end() {
        // No prose after the structure — the model was cut off mid-call. Drop.
        let truncated = "Here's the result:\n<function_calls>\n<invoke name=\"shell\">\n<parameter name=\"command\">sed -n '1,5p' file.rs";
        assert_eq!(strip_tool_call_tags(truncated), "Here's the result:");

        // Cut off mid-tag (no closing '>') is also a truncation.
        let mid_tag = "Working on it <function_calls><invoke name=\"sh";
        assert_eq!(strip_tool_call_tags(mid_tag), "Working on it");
    }

    #[test]
    fn parse_attachment_markers_extracts_known_kinds() {
        let (cleaned, attachments) =
            parse_attachment_markers("Here [IMAGE:/tmp/a.png] and [DOCUMENT:/tmp/b.pdf] done");
        assert_eq!(cleaned, "Here  and  done");
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0], ("IMAGE".into(), "/tmp/a.png".into()));
        assert_eq!(attachments[1], ("DOCUMENT".into(), "/tmp/b.pdf".into()));
    }

    #[test]
    fn parse_attachment_markers_preserves_unknown_kinds() {
        let (cleaned, attachments) = parse_attachment_markers("Check [UNKNOWN:foo] out");
        assert_eq!(cleaned, "Check [UNKNOWN:foo] out");
        assert!(attachments.is_empty());
    }

    #[test]
    fn parse_attachment_markers_preserves_empty_target() {
        let (cleaned, attachments) = parse_attachment_markers("See [IMAGE:] here");
        assert_eq!(cleaned, "See [IMAGE:] here");
        assert!(attachments.is_empty());
    }

    #[test]
    fn parse_attachment_markers_no_markers() {
        let (cleaned, attachments) = parse_attachment_markers("Hello world");
        assert_eq!(cleaned, "Hello world");
        assert!(attachments.is_empty());
    }

    #[test]
    fn parse_attachment_markers_all_kinds() {
        let input = "[IMAGE:a] [PHOTO:b] [DOCUMENT:c] [FILE:d] [VIDEO:e] [AUDIO:f] [VOICE:g]";
        let (_, attachments) = parse_attachment_markers(input);
        assert_eq!(attachments.len(), 7);
    }

    #[test]
    fn parse_attachment_markers_case_insensitive_kind() {
        let (_, attachments) = parse_attachment_markers("[image:/tmp/a.png]");
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].0, "IMAGE");
    }

    #[test]
    fn parse_attachment_markers_of_kinds_leaves_other_kinds_in_text() {
        let (cleaned, attachments) = parse_attachment_markers_of_kinds(
            "Pin [LOCATION:40.7,-74.0] pic [IMAGE:/tmp/a.png]",
            &["LOCATION"],
        );
        assert_eq!(cleaned, "Pin  pic [IMAGE:/tmp/a.png]");
        assert_eq!(
            attachments,
            vec![("LOCATION".to_string(), "40.7,-74.0".to_string())]
        );
    }

    #[test]
    fn location_marker_parses_coordinates_name_and_address() {
        // Bare coordinates.
        assert_eq!(
            WhatsAppLocation::parse("40.7128,-74.0060"),
            Some(WhatsAppLocation {
                lat: 40.7128,
                lng: -74.0060,
                name: None,
                address: None,
            })
        );
        // Coordinates + name, with surrounding whitespace trimmed.
        assert_eq!(
            WhatsAppLocation::parse(" 40.7128 , -74.0060 , Statue of Liberty "),
            Some(WhatsAppLocation {
                lat: 40.7128,
                lng: -74.0060,
                name: Some("Statue of Liberty".to_string()),
                address: None,
            })
        );
        // The address is the trailing field and may contain commas.
        assert_eq!(
            WhatsAppLocation::parse("40.7128,-74.0060,Liberty Island,New York, NY 10004"),
            Some(WhatsAppLocation {
                lat: 40.7128,
                lng: -74.0060,
                name: Some("Liberty Island".to_string()),
                address: Some("New York, NY 10004".to_string()),
            })
        );
        // Double-quoted name may contain commas.
        assert_eq!(
            WhatsAppLocation::parse("40.7128,-74.0060,\"ACME, Inc.\",New York, NY 10004"),
            Some(WhatsAppLocation {
                lat: 40.7128,
                lng: -74.0060,
                name: Some("ACME, Inc.".to_string()),
                address: Some("New York, NY 10004".to_string()),
            })
        );
        // Quoted name without trailing address.
        assert_eq!(
            WhatsAppLocation::parse("40.7128,-74.0060,\"ACME, Inc.\""),
            Some(WhatsAppLocation {
                lat: 40.7128,
                lng: -74.0060,
                name: Some("ACME, Inc.".to_string()),
                address: None,
            })
        );
    }

    #[test]
    fn location_marker_rejects_out_of_range_coordinates() {
        assert_eq!(WhatsAppLocation::parse("91.0,0.0"), None);
        assert_eq!(WhatsAppLocation::parse("-91.0,0.0"), None);
        assert_eq!(WhatsAppLocation::parse("0.0,181.0"), None);
        assert_eq!(WhatsAppLocation::parse("0.0,-181.0"), None);
    }

    #[test]
    fn location_marker_rejects_malformed_input() {
        assert_eq!(WhatsAppLocation::parse("not-a-number,0.0"), None);
        assert_eq!(WhatsAppLocation::parse("40.7128"), None);
        assert_eq!(WhatsAppLocation::parse(""), None);
    }

    #[test]
    fn format_location_content_omits_empty_name() {
        assert_eq!(
            format_location_content(40.7128, -74.0060, Some("NYC")),
            "[Location: 40.712800, -74.006000 — NYC]"
        );
        assert_eq!(
            format_location_content(40.7128, -74.0060, Some("")),
            "[Location: 40.712800, -74.006000]"
        );
        assert_eq!(
            format_location_content(40.7128, -74.0060, None),
            "[Location: 40.712800, -74.006000]"
        );
    }

    #[test]
    fn new_approval_token_is_6_char_alphanumeric() {
        let token = super::new_approval_token();
        assert_eq!(token.len(), 6);
        assert!(token.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn parse_approval_reply_accepts_canonical_forms() {
        use zeroclaw_api::channel::ChannelApprovalResponse;
        let cases = [
            ("abc123 yes", ChannelApprovalResponse::Approve),
            ("abc123 y", ChannelApprovalResponse::Approve),
            ("abc123 approve", ChannelApprovalResponse::Approve),
            ("ABC123 YES", ChannelApprovalResponse::Approve),
            (
                "abc123 yes please go ahead",
                ChannelApprovalResponse::Approve,
            ),
            ("abc123 no", ChannelApprovalResponse::Deny),
            ("abc123 n", ChannelApprovalResponse::Deny),
            ("abc123 deny", ChannelApprovalResponse::Deny),
            ("abc123 always", ChannelApprovalResponse::AlwaysApprove),
        ];
        for (input, expected) in cases {
            let (token, response) = super::parse_approval_reply(input)
                .unwrap_or_else(|| panic!("expected Some for input {:?}", input));
            assert_eq!(
                token,
                input.trim().to_lowercase().split(' ').next().unwrap()
            );
            assert_eq!(response, expected, "input: {input:?}");
        }
    }

    #[test]
    fn parse_approval_reply_rejects_bad_input() {
        let bad = [
            "yes",
            "abc123",
            "abc 123 yes",
            "toolname yes",
            "abc123 maybe",
            "",
            "abc123 ",
        ];
        for input in bad {
            assert!(
                super::parse_approval_reply(input).is_none(),
                "expected None for input {:?}",
                input
            );
        }
    }
}
