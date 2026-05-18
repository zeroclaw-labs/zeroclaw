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
    "IMAGE", "PHOTO", "DOCUMENT", "FILE", "VIDEO", "AUDIO", "VOICE",
];

/// Parse `[KIND:target]` attachment markers out of a message.
/// Returns cleaned text (markers removed) and a vec of `(kind, target)` pairs.
pub fn parse_attachment_markers(message: &str) -> (String, Vec<(String, String)>) {
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
            if target.is_empty() || !ATTACHMENT_KINDS.contains(&kind_upper.as_str()) {
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

/// Generate a short 6-character lowercase alphanumeric approval token.
///
/// Uses the full `[a-z0-9]` alphabet (36 options per position, 36^6 ≈ 2.2B
/// combinations) — not UUID hex (which would give only 16^6 ≈ 16.7M and
/// would materially weaken the WhatsApp no-per-sender-check design
/// described in the PR #6010 security note).
pub(crate) fn new_approval_token() -> String {
    use rand::RngExt;
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..6)
        .map(|_| CHARSET[rng.random_range(0..CHARSET.len())] as char)
        .collect()
}

/// Parse an approval reply of the form `"TOKEN yes|no|always ..."`.
///
/// Returns `Some((token, response))` when the text begins with a 6-character
/// alphanumeric token followed by a recognised action word. Returns `None`
/// for any other input so normal messages are not intercepted.
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
