#[cfg(any(feature = "channel-slack", feature = "channel-telegram"))]
use zeroclaw_api::channel::ProgressEvent;

#[cfg(any(feature = "channel-slack", feature = "channel-telegram"))]
pub(crate) fn lifecycle_progress_fluent_key(event: ProgressEvent) -> &'static str {
    match event {
        ProgressEvent::Received => "channel-runtime-progress-received",
        ProgressEvent::Planning => "channel-runtime-progress-planning",
        ProgressEvent::WaitingOnModel => "channel-runtime-progress-waiting-on-model",
        ProgressEvent::RunningTool => "channel-runtime-progress-running-tool",
        ProgressEvent::CompactingContext => "channel-runtime-progress-compacting-context",
        ProgressEvent::FinalizingResponse => "channel-runtime-progress-finalizing-response",
    }
}

#[cfg(any(feature = "channel-slack", feature = "channel-telegram"))]
pub(crate) fn localized_lifecycle_progress(event: ProgressEvent) -> String {
    zeroclaw_runtime::i18n::get_required_cli_string(lifecycle_progress_fluent_key(event))
}

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

/// Returns the largest UTF-8 character boundary at or before `max_bytes`.
///
/// This compatibility wrapper preserves the previously exported helper while
/// directing new callers to the standard-library implementation.
#[deprecated(since = "0.8.4", note = "use str::floor_char_boundary instead")]
pub fn floor_char_boundary(s: &str, max_bytes: usize) -> usize {
    // Keep downstream callers source-compatible without retaining duplicate boundary logic.
    s.floor_char_boundary(max_bytes)
}

#[cfg(any(feature = "channel-mattermost", feature = "channel-qq"))]
pub(crate) async fn read_response_body_limited(
    mut response: reqwest::Response,
    max_bytes: u64,
) -> anyhow::Result<Vec<u8>> {
    if let Some(content_length) = response.content_length()
        && content_length > max_bytes
    {
        anyhow::bail!(
            "response body content length {content_length} exceeds {max_bytes}-byte limit"
        );
    }

    let mut body = Vec::new();

    while let Some(chunk) = response.chunk().await? {
        let chunk_len = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
        let next_len = u64::try_from(body.len())
            .unwrap_or(u64::MAX)
            .saturating_add(chunk_len);
        if next_len > max_bytes {
            anyhow::bail!("response body exceeds {max_bytes}-byte limit");
        }
        body.extend_from_slice(&chunk);
    }

    Ok(body)
}

#[cfg(all(test, any(feature = "channel-mattermost", feature = "channel-qq")))]
pub(crate) async fn spawn_raw_http_response(
    raw_response: Vec<u8>,
    hold_open: bool,
) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = zeroclaw_spawn::spawn!(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = socket.read(&mut request).await.unwrap();
        socket.write_all(&raw_response).await.unwrap();
        if hold_open {
            std::future::pending::<()>().await;
        }
        socket.shutdown().await.unwrap();
    });

    (format!("http://{address}"), server)
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

pub(crate) const APPROVAL_REPLY_YES: &str = "yes";
pub(crate) const APPROVAL_REPLY_YES_SHORT: &str = "y";
pub(crate) const APPROVAL_REPLY_APPROVE: &str = "approve";
pub(crate) const APPROVAL_REPLY_NO: &str = "no";
pub(crate) const APPROVAL_REPLY_NO_SHORT: &str = "n";
pub(crate) const APPROVAL_REPLY_DENY: &str = "deny";
pub(crate) const APPROVAL_REPLY_ALWAYS: &str = "always";

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
        APPROVAL_REPLY_YES | APPROVAL_REPLY_YES_SHORT | APPROVAL_REPLY_APPROVE => {
            ChannelApprovalResponse::Approve
        }
        APPROVAL_REPLY_NO | APPROVAL_REPLY_NO_SHORT | APPROVAL_REPLY_DENY => {
            ChannelApprovalResponse::Deny
        }
        APPROVAL_REPLY_ALWAYS => ChannelApprovalResponse::AlwaysApprove,
        _ => return None,
    };
    Some((token, response))
}

/// Localized text-reply approval prompt using yes/no/always reply keywords:
/// Discord's plaintext fallback, Signal, WhatsApp, and Slack's polling-mode
/// fallback all send this exact shape. The heading/labels/instruction come
/// from the runtime Fluent catalogue; `token`/`tool_name`/`arguments_summary`
/// are protocol-exact values echoed verbatim — never localized — so a locale
/// switch cannot desync the prompt from [`parse_approval_reply`].
#[cfg(any(
    feature = "channel-discord",
    feature = "channel-signal",
    feature = "channel-slack",
    feature = "channel-whatsapp-cloud",
    feature = "whatsapp-web",
    test
))]
pub(crate) fn build_yesno_approval_prompt(
    token: &str,
    tool_name: &str,
    arguments_summary: &str,
) -> String {
    let heading = zeroclaw_runtime::i18n::get_required_cli_string("channel-approval-heading-shout");
    let tool_label = zeroclaw_runtime::i18n::get_required_cli_string("channel-approval-tool-label");
    let args_label = zeroclaw_runtime::i18n::get_required_cli_string("channel-approval-args-label");
    let yes_command = format!("{token} {APPROVAL_REPLY_YES}");
    let no_command = format!("{token} {APPROVAL_REPLY_NO}");
    let always_command = format!("{token} {APPROVAL_REPLY_ALWAYS}");
    let reply = zeroclaw_runtime::i18n::get_required_cli_string_with_args(
        "channel-approval-reply-instruction-yesno",
        &[
            ("yes_command", yes_command.as_str()),
            ("no_command", no_command.as_str()),
            ("always_command", always_command.as_str()),
        ],
    );
    format!(
        "{heading} [{token}]\n{tool_label}: {tool_name}\n{args_label}: {arguments_summary}\n\n{reply}"
    )
}

/// Localized text-reply approval prompt using approve/deny/always reply
/// keywords: Matrix's own reply parser (distinct from
/// [`parse_approval_reply`]) expects this shape.
#[cfg(any(feature = "channel-matrix", test))]
pub(crate) fn build_approve_deny_approval_prompt(
    token: &str,
    tool_name: &str,
    arguments_summary: &str,
) -> String {
    let heading = zeroclaw_runtime::i18n::get_required_cli_string("channel-approval-heading-shout");
    let tool_label = zeroclaw_runtime::i18n::get_required_cli_string("channel-approval-tool-label");
    let args_label = zeroclaw_runtime::i18n::get_required_cli_string("channel-approval-args-label");
    let approve_command = format!("{token} {APPROVAL_REPLY_APPROVE}");
    let deny_command = format!("{token} {APPROVAL_REPLY_DENY}");
    let always_command = format!("{token} {APPROVAL_REPLY_ALWAYS}");
    let reply = zeroclaw_runtime::i18n::get_required_cli_string_with_args(
        "channel-approval-reply-instruction-approve-deny",
        &[
            ("approve_command", approve_command.as_str()),
            ("deny_command", deny_command.as_str()),
            ("always_command", always_command.as_str()),
        ],
    );
    format!(
        "{heading} [{token}]\n{tool_label}: {tool_name}\n{args_label}: {arguments_summary}\n\n{reply}"
    )
}

#[cfg(any(
    feature = "channel-matrix",
    feature = "channel-slack",
    feature = "channel-telegram",
    test
))]
pub(crate) struct PendingApproval {
    pub(crate) sender: tokio::sync::oneshot::Sender<zeroclaw_api::channel::ChannelApprovalResponse>,
    pub(crate) destination: String,
    pub(crate) tool_name: String,
}

#[cfg(any(
    feature = "channel-matrix",
    feature = "channel-slack",
    feature = "channel-telegram",
    test
))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PendingApprovalResolution {
    NotFound,
    Rejected,
    Resolved,
    ReceiverClosed,
}

#[cfg(any(
    feature = "channel-matrix",
    feature = "channel-slack",
    feature = "channel-telegram",
    test
))]
impl PendingApprovalResolution {
    #[cfg(any(feature = "channel-matrix", feature = "channel-slack", test))]
    pub(crate) fn suppresses_message(self) -> bool {
        !matches!(self, Self::NotFound)
    }
}

#[cfg(any(feature = "channel-matrix", feature = "channel-slack", test))]
pub(crate) async fn resolve_pending_approval(
    pending_approvals: &tokio::sync::Mutex<std::collections::HashMap<String, PendingApproval>>,
    token: &str,
    response: zeroclaw_api::channel::ChannelApprovalResponse,
    responder_allowed: bool,
    destination: &str,
) -> PendingApprovalResolution {
    resolve_pending_approval_with_tool(
        pending_approvals,
        token,
        response,
        responder_allowed,
        destination,
    )
    .await
    .0
}

#[cfg(any(
    feature = "channel-matrix",
    feature = "channel-slack",
    feature = "channel-telegram",
    test
))]
pub(crate) async fn resolve_pending_approval_with_tool(
    pending_approvals: &tokio::sync::Mutex<std::collections::HashMap<String, PendingApproval>>,
    token: &str,
    response: zeroclaw_api::channel::ChannelApprovalResponse,
    responder_allowed: bool,
    destination: &str,
) -> (PendingApprovalResolution, Option<String>) {
    let mut pending_approvals = pending_approvals.lock().await;
    let Some(pending) = pending_approvals.get(token) else {
        return (PendingApprovalResolution::NotFound, None);
    };
    if !responder_allowed || destination.is_empty() || pending.destination != destination {
        return (PendingApprovalResolution::Rejected, None);
    }

    let Some(pending) = pending_approvals.remove(token) else {
        return (PendingApprovalResolution::NotFound, None);
    };
    drop(pending_approvals);
    if pending.sender.send(response).is_ok() {
        (PendingApprovalResolution::Resolved, Some(pending.tool_name))
    } else {
        (PendingApprovalResolution::ReceiverClosed, None)
    }
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

    /// Verifies the exported compatibility wrapper retains the legacy UTF-8 boundary contract.
    #[allow(deprecated)]
    #[test]
    fn floor_char_boundary_compatibility_wrapper_delegates_to_std() {
        let text = "abc😀def";

        assert_eq!(floor_char_boundary(text, 5), 3);
        assert_eq!(floor_char_boundary(text, usize::MAX), text.len());
    }

    #[cfg(any(feature = "channel-mattermost", feature = "channel-qq"))]
    async fn response_from_raw_http(
        raw_response: Vec<u8>,
        hold_open: bool,
    ) -> (reqwest::Response, tokio::task::JoinHandle<()>) {
        let (url, server) = spawn_raw_http_response(raw_response, hold_open).await;
        let response = reqwest::get(url).await.unwrap();
        (response, server)
    }

    #[cfg(any(feature = "channel-mattermost", feature = "channel-qq"))]
    #[tokio::test]
    async fn bounded_response_body_rejects_declared_oversize() {
        let (response, server) = response_from_raw_http(
            b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\n".to_vec(),
            true,
        )
        .await;

        let error = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            read_response_body_limited(response, 5),
        )
        .await
        .expect("declared oversize must be rejected before reading the body")
        .unwrap_err();
        server.abort();

        assert!(
            error
                .to_string()
                .contains("content length 6 exceeds 5-byte limit")
        );
    }

    #[cfg(any(feature = "channel-mattermost", feature = "channel-qq"))]
    #[tokio::test]
    async fn bounded_response_body_accepts_chunked_body_at_limit() {
        let (response, server) = response_from_raw_http(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2\r\nab\r\n3\r\ncde\r\n0\r\n\r\n".to_vec(),
            false,
        )
        .await;

        let body = read_response_body_limited(response, 5).await.unwrap();
        server.await.unwrap();

        assert_eq!(body, b"abcde");
    }

    #[cfg(any(feature = "channel-mattermost", feature = "channel-qq"))]
    #[tokio::test]
    async fn bounded_response_body_rejects_chunked_body_over_limit() {
        let (response, server) = response_from_raw_http(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n3\r\ndef\r\n".to_vec(),
            true,
        )
        .await;

        let error = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            read_response_body_limited(response, 5),
        )
        .await
        .expect("chunked oversize must be rejected before the response ends")
        .unwrap_err();
        server.abort();

        assert!(error.to_string().contains("exceeds 5-byte limit"));
    }

    #[cfg(any(feature = "channel-slack", feature = "channel-telegram"))]
    #[test]
    fn lifecycle_progress_maps_typed_events_to_fluent_keys() {
        assert_eq!(
            [
                ProgressEvent::Received,
                ProgressEvent::Planning,
                ProgressEvent::WaitingOnModel,
                ProgressEvent::RunningTool,
                ProgressEvent::CompactingContext,
                ProgressEvent::FinalizingResponse,
            ]
            .map(lifecycle_progress_fluent_key),
            [
                "channel-runtime-progress-received",
                "channel-runtime-progress-planning",
                "channel-runtime-progress-waiting-on-model",
                "channel-runtime-progress-running-tool",
                "channel-runtime-progress-compacting-context",
                "channel-runtime-progress-finalizing-response",
            ]
        );
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

    #[test]
    fn yesno_approval_prompt_keywords_still_parse_via_parse_approval_reply() {
        use zeroclaw_api::channel::ChannelApprovalResponse;

        // Localization must not desync the prompt text from
        // `parse_approval_reply`: whatever locale is active, the reply
        // keywords embedded in the prompt prose must remain the literal
        // ASCII words the parser expects. Exercises the exact prompt shared
        // by Discord's plaintext fallback, Signal, WhatsApp, and Slack's
        // polling-mode fallback.
        let token = "ab12cd";
        let prompt = super::build_yesno_approval_prompt(token, "shell", "ls -la");
        assert!(
            prompt.contains(token),
            "prompt should echo the token verbatim; got {prompt:?}"
        );
        assert!(
            prompt.contains("shell") && prompt.contains("ls -la"),
            "prompt should echo the tool name and args verbatim; got {prompt:?}"
        );

        for (word, expected) in [
            ("yes", ChannelApprovalResponse::Approve),
            ("no", ChannelApprovalResponse::Deny),
            ("always", ChannelApprovalResponse::AlwaysApprove),
        ] {
            let reply = format!("{token} {word}");
            assert!(
                prompt.contains(&reply),
                "prompt should show the exact reply {reply:?}; got {prompt:?}"
            );
            let (parsed_token, response) = super::parse_approval_reply(&reply)
                .unwrap_or_else(|| panic!("{reply:?} should parse"));
            assert_eq!(parsed_token, token);
            assert_eq!(response, expected);
        }
    }

    #[test]
    fn approve_deny_approval_prompt_matches_matrix_own_parser_keywords() {
        // Same desync guard as above, for Matrix's `approve`/`deny`/`always`
        // reply shape (Matrix uses its own parser, not
        // `parse_approval_reply`, but the keyword contract is identical).
        let token = "AB12CD34";
        let prompt = super::build_approve_deny_approval_prompt(token, "shell", "ls -la");
        assert!(prompt.contains(token));
        for word in ["approve", "deny", "always"] {
            let reply = format!("{token} {word}");
            assert!(
                prompt.contains(&reply),
                "prompt should show the exact reply {reply:?}; got {prompt:?}"
            );
        }
    }

    #[test]
    fn literal_approval_keys_in_listed_adapter_sources_resolve() {
        // Heuristic smoke guard for literal Fluent keys in the current
        // adapter list. It complements, but does not replace, compile-time
        // feature coverage or the explicit catalogue-key tests.
        // `include_str!` embeds each source at compile time regardless of its
        // `#[cfg(feature = ...)]`, so this always-compiled test can catch
        // literal-key typos in feature-gated sources.
        const SOURCES: &[&str] = &[
            include_str!("telegram.rs"),
            include_str!("discord/mod.rs"),
            include_str!("discord/approval.rs"),
            include_str!("slack.rs"),
            include_str!("matrix.rs"),
            include_str!("signal.rs"),
            include_str!("whatsapp.rs"),
            include_str!("whatsapp_web.rs"),
            include_str!("acp_channel.rs"),
            include_str!("util.rs"),
        ];
        let mut keys = std::collections::BTreeSet::new();
        for src in SOURCES {
            // Capture the quoted first argument of every runtime lookup call
            // (`get_required_cli_string` also prefixes the `_with_args`
            // form), keeping only approval keys.
            for (idx, _) in src.match_indices("get_required_cli_string") {
                let rest = &src[idx..];
                let Some(open) = rest.find('"') else {
                    continue;
                };
                let after = &rest[open + 1..];
                let Some(close) = after.find('"') else {
                    continue;
                };
                let key = &after[..close];
                if key.starts_with("channel-") && key.contains("approval") {
                    keys.insert(key.to_string());
                }
            }
        }
        assert!(
            keys.len() >= 18,
            "expected to scan the shared approval keys across adapters; found only {} ({keys:?}) — the scanner is likely broken",
            keys.len()
        );
        for key in &keys {
            // Supply dummy values for every arg any approval key might take.
            // Fluent ignores args a message doesn't reference, so no-arg keys
            // still resolve; arg-requiring messages resolved with no args
            // would otherwise fail to format and false-positive here.
            let resolved = zeroclaw_runtime::i18n::get_required_cli_string_with_args(
                key,
                &[
                    ("tool", "TOOL"),
                    ("yes_command", "TKN yes"),
                    ("no_command", "TKN no"),
                    ("approve_command", "TKN approve"),
                    ("deny_command", "TKN deny"),
                    ("always_command", "TKN always"),
                ],
            );
            assert_ne!(
                resolved,
                format!("{{{key}}}"),
                "adapter source references Fluent key {key:?}, but it resolves to the missing-string sentinel (undefined or typo'd)"
            );
        }
    }

    #[tokio::test]
    async fn approval_resolution_requires_authorized_responder_and_destination() {
        use zeroclaw_api::channel::ChannelApprovalResponse;

        let pending = tokio::sync::Mutex::new(std::collections::HashMap::new());
        let (tx, rx) = tokio::sync::oneshot::channel();
        pending.lock().await.insert(
            "approval-id".to_string(),
            PendingApproval {
                sender: tx,
                destination: "room-a".to_string(),
                tool_name: "tool".to_string(),
            },
        );

        let rejected_responder = resolve_pending_approval(
            &pending,
            "approval-id",
            ChannelApprovalResponse::Approve,
            false,
            "room-a",
        )
        .await;
        assert_eq!(rejected_responder, PendingApprovalResolution::Rejected);
        assert!(
            rejected_responder.suppresses_message(),
            "a rejected reply for a known approval must not reach normal dispatch"
        );
        assert!(pending.lock().await.contains_key("approval-id"));

        let rejected_destination = resolve_pending_approval(
            &pending,
            "approval-id",
            ChannelApprovalResponse::Deny,
            true,
            "room-b",
        )
        .await;
        assert_eq!(rejected_destination, PendingApprovalResolution::Rejected);
        assert!(
            rejected_destination.suppresses_message(),
            "a cross-destination reply for a known approval must not reach normal dispatch"
        );
        assert!(pending.lock().await.contains_key("approval-id"));

        let resolved = resolve_pending_approval(
            &pending,
            "approval-id",
            ChannelApprovalResponse::AlwaysApprove,
            true,
            "room-a",
        )
        .await;
        assert_eq!(resolved, PendingApprovalResolution::Resolved);
        assert!(resolved.suppresses_message());
        assert_eq!(rx.await.unwrap(), ChannelApprovalResponse::AlwaysApprove);
        assert!(pending.lock().await.is_empty());

        let not_found = resolve_pending_approval(
            &pending,
            "missing-approval-id",
            ChannelApprovalResponse::Approve,
            true,
            "room-a",
        )
        .await;
        assert_eq!(not_found, PendingApprovalResolution::NotFound);
        assert!(
            !not_found.suppresses_message(),
            "ordinary text must continue when no pending approval owns its token"
        );
    }

    #[tokio::test]
    async fn approval_resolution_reports_closed_receiver_as_failure() {
        use zeroclaw_api::channel::ChannelApprovalResponse;

        let pending = tokio::sync::Mutex::new(std::collections::HashMap::new());
        let (tx, rx) = tokio::sync::oneshot::channel();
        drop(rx);
        pending.lock().await.insert(
            "approval-id".to_string(),
            PendingApproval {
                sender: tx,
                destination: "room-a".to_string(),
                tool_name: "tool".to_string(),
            },
        );

        let resolution = resolve_pending_approval(
            &pending,
            "approval-id",
            ChannelApprovalResponse::Approve,
            true,
            "room-a",
        )
        .await;
        assert_eq!(resolution, PendingApprovalResolution::ReceiverClosed);
        assert!(
            resolution.suppresses_message(),
            "a consumed approval must not fall through after its receiver closes"
        );
        assert!(pending.lock().await.is_empty());
    }
}
