#[cfg(any(
    test,
    feature = "channel-discord",
    feature = "channel-lark",
    feature = "channel-matrix",
    feature = "channel-mattermost",
    feature = "channel-signal",
    feature = "channel-slack",
    feature = "channel-telegram",
    feature = "channel-whatsapp-cloud",
    feature = "whatsapp-web"
))]
use std::{collections::HashMap, sync::Arc};

#[cfg(test)]
pub(crate) mod approval_audit_test_support;

#[cfg(any(
    test,
    feature = "channel-discord",
    feature = "channel-lark",
    feature = "channel-matrix",
    feature = "channel-signal",
    feature = "channel-slack",
    feature = "channel-telegram",
    feature = "channel-whatsapp-cloud",
    feature = "whatsapp-web"
))]
pub(crate) trait ApprovalRegistration {
    fn registration_id(&self) -> uuid::Uuid;
}

#[cfg(test)]
impl ApprovalRegistration for uuid::Uuid {
    fn registration_id(&self) -> uuid::Uuid {
        *self
    }
}

/// Removes a pending approval if its requesting future is cancelled.
///
/// The guard is armed immediately after registration. Normal cleanup removes
/// the entry before disarming so cancellation while waiting for the map lock
/// cannot leave a stale token behind.
#[cfg(any(
    test,
    feature = "channel-discord",
    feature = "channel-lark",
    feature = "channel-matrix",
    feature = "channel-signal",
    feature = "channel-slack",
    feature = "channel-telegram",
    feature = "channel-whatsapp-cloud",
    feature = "whatsapp-web"
))]
pub(crate) struct PendingApprovalGuard<V: ApprovalRegistration + Send + 'static> {
    pending: Arc<tokio::sync::Mutex<HashMap<String, V>>>,
    token: Option<String>,
    registration_id: uuid::Uuid,
}

#[cfg(any(
    test,
    feature = "channel-discord",
    feature = "channel-lark",
    feature = "channel-matrix",
    feature = "channel-signal",
    feature = "channel-slack",
    feature = "channel-telegram",
    feature = "channel-whatsapp-cloud",
    feature = "whatsapp-web"
))]
impl<V: ApprovalRegistration + Send + 'static> PendingApprovalGuard<V> {
    pub(crate) fn new(
        pending: Arc<tokio::sync::Mutex<HashMap<String, V>>>,
        token: String,
        registration_id: uuid::Uuid,
    ) -> Self {
        Self {
            pending,
            token: Some(token),
            registration_id,
        }
    }

    /// The inbound handler removed the entry, or cleanup removed it normally.
    pub(crate) fn disarm(&mut self) {
        self.token = None;
    }

    #[cfg(feature = "channel-slack")]
    pub(crate) fn registration_id(&self) -> uuid::Uuid {
        self.registration_id
    }

    /// Remove the pending entry before disarming the cancellation guard.
    pub(crate) async fn remove(&mut self) {
        let Some(token) = self.token.as_deref() else {
            return;
        };
        let mut pending = self.pending.lock().await;
        if pending
            .get(token)
            .is_some_and(|entry| entry.registration_id() == self.registration_id)
        {
            pending.remove(token);
        }
        self.token = None;
    }
}

#[cfg(any(
    test,
    feature = "channel-discord",
    feature = "channel-lark",
    feature = "channel-matrix",
    feature = "channel-signal",
    feature = "channel-slack",
    feature = "channel-telegram",
    feature = "channel-whatsapp-cloud",
    feature = "whatsapp-web"
))]
impl<V: ApprovalRegistration + Send + 'static> Drop for PendingApprovalGuard<V> {
    fn drop(&mut self) {
        let Some(token) = self.token.take() else {
            return;
        };
        let pending = Arc::clone(&self.pending);
        let registration_id = self.registration_id;
        let removed = if let Ok(mut entries) = pending.try_lock() {
            if entries
                .get(&token)
                .is_some_and(|entry| entry.registration_id() == registration_id)
            {
                entries.remove(&token);
            }
            true
        } else {
            false
        };
        if removed {
            return;
        }
        zeroclaw_spawn::spawn!(async move {
            let mut entries = pending.lock().await;
            if entries
                .get(&token)
                .is_some_and(|entry| entry.registration_id() == registration_id)
            {
                entries.remove(&token);
            }
        });
    }
}

/// A pending approval owns this capability. The prepared tool remains the
/// source of its request, invocation and session identities; transport reply
/// tokens never select a separate process-wide writer.
#[cfg(any(
    feature = "channel-discord",
    feature = "channel-lark",
    feature = "channel-matrix",
    feature = "channel-mattermost",
    feature = "channel-signal",
    feature = "channel-slack",
    feature = "channel-telegram",
    feature = "channel-whatsapp-cloud",
    feature = "whatsapp-web",
    test
))]
#[derive(Clone, Default)]
pub(crate) struct ApprovalAudit {
    prepared: Option<Arc<dyn zeroclaw_api::session_events::capture::PreparedToolPort>>,
}

#[cfg(any(
    feature = "channel-discord",
    feature = "channel-lark",
    feature = "channel-matrix",
    feature = "channel-mattermost",
    feature = "channel-signal",
    feature = "channel-slack",
    feature = "channel-telegram",
    feature = "channel-whatsapp-cloud",
    feature = "whatsapp-web",
    test
))]
impl ApprovalAudit {
    pub(crate) fn current() -> Self {
        Self {
            prepared: zeroclaw_api::session_events::capture::current_tool_preparation(),
        }
    }

    pub(crate) async fn record_rejection(
        &self,
        classification: &str,
        evidence: serde_json::Value,
    ) -> anyhow::Result<()> {
        if let Some(prepared) = &self.prepared {
            prepared
                .record_approval_reply_rejected(classification.to_owned(), evidence)
                .await?;
        }
        Ok(())
    }

    async fn record_reply(
        &self,
        actor: zeroclaw_api::session_events::EventActor,
        response: zeroclaw_api::channel::ChannelApprovalResponse,
        evidence: serde_json::Value,
    ) -> anyhow::Result<zeroclaw_api::channel::AttributedApprovalResponse> {
        let reply_event_id = if let Some(prepared) = &self.prepared {
            Some(
                prepared
                    .record_approval_reply(actor, response.clone(), evidence)
                    .await?,
            )
        } else {
            None
        };
        Ok(
            zeroclaw_api::channel::AttributedApprovalResponse::operator(response)
                .with_reply_event_id(reply_event_id),
        )
    }

    pub(crate) async fn deliver_reply(
        &self,
        sender: tokio::sync::oneshot::Sender<zeroclaw_api::channel::ApprovalReplyResult>,
        actor: zeroclaw_api::session_events::EventActor,
        response: zeroclaw_api::channel::ChannelApprovalResponse,
        evidence: serde_json::Value,
    ) -> anyhow::Result<bool> {
        let reply = match self.record_reply(actor, response, evidence).await {
            Ok(reply) => reply,
            Err(error) => {
                let (callback_error, waiter_error) =
                    zeroclaw_api::session_events::capture::share_capture_failure(error);
                let _ = sender.send(Err(waiter_error));
                return Err(callback_error);
            }
        };
        Ok(sender.send(Ok(reply)).is_ok())
    }
}

#[cfg(any(
    feature = "channel-discord",
    feature = "channel-lark",
    feature = "channel-matrix",
    feature = "channel-mattermost",
    feature = "channel-signal",
    feature = "channel-slack",
    feature = "channel-telegram",
    feature = "channel-whatsapp-cloud",
    feature = "whatsapp-web",
    test
))]
pub(crate) fn approval_reply_actor(
    channel: &impl zeroclaw_api::attribution::Attributable,
    responder_id: &str,
) -> zeroclaw_api::session_events::EventActor {
    use zeroclaw_api::session_events::EventActor;
    if responder_id.trim().is_empty() || channel.alias().trim().is_empty() {
        return EventActor::System {
            component: "approval_identity_unavailable".into(),
        };
    }
    EventActor::ChannelResponder {
        channel: format!(
            "{}.{}",
            channel.role().composite_type().unwrap_or("channel"),
            channel.alias()
        ),
        responder_id: responder_id.into(),
    }
}

#[cfg(any(
    feature = "channel-discord",
    feature = "channel-matrix",
    feature = "channel-signal",
    feature = "channel-slack",
    feature = "channel-telegram",
    feature = "whatsapp-web",
    test
))]
pub(crate) struct ApprovalReply {
    pub(crate) response: zeroclaw_api::channel::ChannelApprovalResponse,
    pub(crate) actor: zeroclaw_api::session_events::EventActor,
}

#[cfg(any(
    feature = "channel-discord",
    feature = "channel-matrix",
    feature = "channel-signal",
    feature = "channel-slack",
    feature = "channel-telegram",
    test
))]
impl ApprovalReply {
    pub(crate) fn authenticated(
        response: zeroclaw_api::channel::ChannelApprovalResponse,
        channel: &impl zeroclaw_api::attribution::Attributable,
        responder_id: &str,
    ) -> Self {
        Self {
            response,
            actor: approval_reply_actor(channel, responder_id),
        }
    }
}

#[cfg(any(
    feature = "channel-discord",
    feature = "channel-matrix",
    feature = "channel-signal",
    feature = "channel-slack",
    feature = "channel-telegram",
    feature = "whatsapp-web",
    test
))]
impl From<zeroclaw_api::channel::ChannelApprovalResponse> for ApprovalReply {
    fn from(response: zeroclaw_api::channel::ChannelApprovalResponse) -> Self {
        Self {
            response,
            actor: zeroclaw_api::session_events::EventActor::System {
                component: "approval_identity_unavailable".into(),
            },
        }
    }
}

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

/// Minimum reply size, in bytes, that earns a voice note. Byte-measured, so a
/// non-ASCII reply clears the floor with fewer characters than an ASCII one.
#[cfg(any(feature = "channel-telegram", feature = "whatsapp-web", test))]
const MIN_VOICE_REPLY_BYTES: usize = 40;

/// Bytes allowed between the brackets of a leading expressive audio tag. Real
/// tags are short (`[whispers]`, `[strong French accent]`); the bound keeps a
/// long bracketed block from passing as one.
#[cfg(any(feature = "channel-telegram", feature = "whatsapp-web", test))]
const MAX_AUDIO_TAG_INNER_BYTES: usize = 32;

/// Classify a reply that opens with `[`. Returns the skip reason when the
/// bracketed run is machine output, or `None` when it is an expressive audio
/// tag (`[whispers]`, `[very excited]`) decorating real prose.
///
/// Audio tags are stage directions that TTS engines interpret rather than
/// speak, so a reply opening with one is prose and belongs in a voice note.
#[cfg(any(feature = "channel-telegram", feature = "whatsapp-web", test))]
fn leading_bracket_skip_reason(content: &str) -> Option<&'static str> {
    let after_open = content.strip_prefix('[')?;

    // JSON-looking openers: `[{`, `["`, `[0`-`[9`, `[]`.
    if after_open.starts_with(|c: char| matches!(c, '{' | '"' | ']') || c.is_ascii_digit()) {
        return Some("json_array");
    }

    // No closing bracket at all, so nothing identifies this as a tag.
    let Some(close) = after_open.find(']') else {
        return Some("unclosed_bracket");
    };
    let inner = &after_open[..close];
    let tail = &after_open[close + 1..];

    // Markdown link: `[text](url)`. Without this the URL would be spoken.
    if tail.starts_with('(') {
        return Some("markdown_link");
    }
    // Too long to be a stage direction, so treat it as an unknown bracketed
    // block and keep the pre-existing rejection rather than guessing.
    if inner.len() > MAX_AUDIO_TAG_INNER_BYTES {
        return Some("bracketed_prefix");
    }

    None
}

/// Why a reply was not queued as a TTS voice note, or `None` when it is worth
/// speaking. Voice chats mirror the agent's prose, not its plumbing: URLs,
/// JSON, code blocks, raw tool output and one-line status make poor audio.
#[cfg(any(feature = "channel-telegram", feature = "whatsapp-web", test))]
pub(crate) fn voice_reply_skip_reason(content: &str) -> Option<&'static str> {
    if content.len() <= MIN_VOICE_REPLY_BYTES {
        return Some("too_short");
    }
    if content.starts_with("http") {
        return Some("url_prefix");
    }
    if content.starts_with('{') {
        return Some("json_object");
    }
    if let Some(reason) = leading_bracket_skip_reason(content) {
        return Some(reason);
    }
    if content.starts_with("Error") {
        return Some("error_prefix");
    }
    if content.contains("```") {
        return Some("code_fence");
    }
    if content.contains("tool_call") {
        return Some("tool_call_marker");
    }
    if content.contains("wttr.in") {
        return Some("weather_tool_output");
    }
    None
}

#[cfg(any(feature = "whatsapp-web", feature = "channel-whatsapp-cloud"))]
pub(crate) use zeroclaw_api::media::LocationAttachment as WhatsAppLocation;

/// Render an inbound static location as chat text, e.g.
/// `Location: 40.712800, -74.006000 — NYC`. Shared by both WhatsApp
/// backends so inbound pins read identically regardless of transport.
#[cfg(any(feature = "whatsapp-web", feature = "channel-whatsapp-cloud", test))]
pub(crate) fn format_location_content(lat: f64, lng: f64, name: Option<&str>) -> String {
    match name.filter(|n| !n.is_empty()) {
        Some(name) => format!("Location: {lat:.6}, {lng:.6} — {name}"),
        None => format!("Location: {lat:.6}, {lng:.6}"),
    }
}

#[cfg(any(
    feature = "channel-discord",
    feature = "channel-mattermost",
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
    feature = "channel-mattermost",
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
    position: Option<(u32, u32)>,
) -> String {
    let position_line = approval_position_line(position);
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
        "{heading} [{token}]\n{position_line}{tool_label}: {tool_name}\n{args_label}: {arguments_summary}\n\n{reply}"
    )
}

/// Localized `Tool call 2 of 3` line for an approval prompt, already
/// newline-terminated, or empty when there is no counter to show.
///
/// Kept here so every adapter renders the same wording from the same
/// catalogue key and the phrasing cannot drift per channel.
#[cfg(any(
    feature = "channel-discord",
    feature = "channel-mattermost",
    feature = "channel-signal",
    feature = "channel-slack",
    feature = "channel-whatsapp-cloud",
    feature = "whatsapp-web",
    feature = "channel-matrix",
    feature = "channel-telegram",
    feature = "channel-lark",
    test
))]
pub(crate) fn approval_position_line(position: Option<(u32, u32)>) -> String {
    match position {
        // `1 of 1` tells the operator nothing they did not already know. The
        // rule lives here so no adapter has to remember it.
        Some((_, total)) if total <= 1 => String::new(),
        Some((index, total)) => {
            let text = zeroclaw_runtime::i18n::get_required_cli_string_with_args(
                "channel-approval-position",
                &[
                    ("index", index.to_string().as_str()),
                    ("total", total.to_string().as_str()),
                ],
            );
            format!("{text}\n")
        }
        None => String::new(),
    }
}

/// Localized text-reply approval prompt using approve/deny/always reply
/// keywords: Matrix's own reply parser (distinct from
/// [`parse_approval_reply`]) expects this shape.
#[cfg(any(feature = "channel-matrix", test))]
pub(crate) fn build_approve_deny_approval_prompt(
    token: &str,
    tool_name: &str,
    arguments_summary: &str,
    position: Option<(u32, u32)>,
) -> String {
    let position_line = approval_position_line(position);
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
        "{heading} [{token}]\n{position_line}{tool_label}: {tool_name}\n{args_label}: {arguments_summary}\n\n{reply}"
    )
}

#[cfg(any(
    feature = "channel-discord",
    feature = "channel-matrix",
    feature = "channel-signal",
    feature = "channel-slack",
    feature = "channel-telegram",
    test
))]
pub(crate) struct PendingApproval {
    pub(crate) registration_id: uuid::Uuid,
    pub(crate) audit: ApprovalAudit,
    pub(crate) sender: tokio::sync::oneshot::Sender<zeroclaw_api::channel::ApprovalReplyResult>,
    pub(crate) destination: String,
    pub(crate) tool_name: String,
}

#[cfg(any(
    test,
    feature = "channel-discord",
    feature = "channel-matrix",
    feature = "channel-signal",
    feature = "channel-slack",
    feature = "channel-telegram"
))]
impl ApprovalRegistration for PendingApproval {
    fn registration_id(&self) -> uuid::Uuid {
        self.registration_id
    }
}

#[cfg(any(
    test,
    feature = "channel-discord",
    feature = "channel-matrix",
    feature = "channel-signal",
    feature = "channel-slack",
    feature = "channel-telegram"
))]
pub(crate) async fn register_pending_approval(
    pending: Arc<tokio::sync::Mutex<HashMap<String, PendingApproval>>>,
    token: String,
    sender: tokio::sync::oneshot::Sender<zeroclaw_api::channel::ApprovalReplyResult>,
    destination: String,
    tool_name: String,
) -> anyhow::Result<PendingApprovalGuard<PendingApproval>> {
    let registration_id = uuid::Uuid::new_v4();
    let audit = ApprovalAudit::current();
    {
        let mut entries = pending.lock().await;
        let std::collections::hash_map::Entry::Vacant(entry) = entries.entry(token.clone()) else {
            anyhow::bail!("approval_token_already_registered");
        };
        entry.insert(PendingApproval {
            registration_id,
            audit,
            sender,
            destination,
            tool_name,
        });
    }
    // No suspension between insertion and arming the owner of that registration.
    Ok(PendingApprovalGuard::new(pending, token, registration_id))
}

#[cfg(any(
    feature = "channel-discord",
    feature = "channel-matrix",
    feature = "channel-signal",
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
    feature = "channel-discord",
    feature = "channel-matrix",
    feature = "channel-signal",
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

#[cfg(any(
    feature = "channel-discord",
    feature = "channel-matrix",
    feature = "channel-signal",
    feature = "channel-slack",
    test
))]
pub(crate) async fn resolve_pending_approval<R: Into<ApprovalReply> + Send>(
    pending_approvals: &tokio::sync::Mutex<std::collections::HashMap<String, PendingApproval>>,
    token: &str,
    response: R,
    responder_allowed: bool,
    destination: &str,
) -> anyhow::Result<PendingApprovalResolution> {
    Ok(resolve_pending_approval_with_tool(
        pending_approvals,
        token,
        response,
        responder_allowed,
        destination,
    )
    .await?
    .0)
}

#[cfg(any(
    feature = "channel-discord",
    feature = "channel-matrix",
    feature = "channel-signal",
    feature = "channel-slack",
    feature = "channel-telegram",
    test
))]
pub(crate) async fn resolve_pending_approval_with_tool<R: Into<ApprovalReply> + Send>(
    pending_registry: &tokio::sync::Mutex<std::collections::HashMap<String, PendingApproval>>,
    token: &str,
    response: R,
    responder_allowed: bool,
    destination: &str,
) -> anyhow::Result<(PendingApprovalResolution, Option<String>)> {
    resolve_pending_approval_generation(
        pending_registry,
        token,
        None,
        response,
        responder_allowed,
        destination,
    )
    .await
}

#[cfg(any(
    feature = "channel-discord",
    feature = "channel-matrix",
    feature = "channel-signal",
    feature = "channel-slack",
    feature = "channel-telegram",
    test
))]
pub(crate) async fn resolve_pending_approval_generation<R: Into<ApprovalReply> + Send>(
    pending_registry: &tokio::sync::Mutex<std::collections::HashMap<String, PendingApproval>>,
    token: &str,
    registration_id: Option<uuid::Uuid>,
    response: R,
    responder_allowed: bool,
    destination: &str,
) -> anyhow::Result<(PendingApprovalResolution, Option<String>)> {
    let reply = response.into();
    let mut pending_approvals = pending_registry.lock().await;
    let Some(pending) = pending_approvals.get(token) else {
        return Ok((PendingApprovalResolution::NotFound, None));
    };
    if registration_id.is_some_and(|expected| expected != pending.registration_id) {
        return Ok((PendingApprovalResolution::NotFound, None));
    }
    if !responder_allowed || destination.is_empty() || pending.destination != destination {
        let expected_destination = pending.destination.clone();
        let audit = pending.audit.clone();
        drop(pending_approvals);
        audit
            .record_rejection(
                if !responder_allowed {
                    "unauthorized_responder"
                } else {
                    "wrong_destination"
                },
                serde_json::json!({
                    "destination": destination,
                    "expected_destination": expected_destination,
                    "responder_allowed": responder_allowed,
                }),
            )
            .await?;
        return Ok((PendingApprovalResolution::Rejected, None));
    }

    if pending.sender.is_closed() {
        let audit = pending.audit.clone();
        let registration_id = pending.registration_id;
        drop(pending_approvals);
        audit
            .record_rejection(
                "receiver_closed",
                serde_json::json!({"destination": destination}),
            )
            .await?;
        let mut entries = pending_registry.lock().await;
        if entries
            .get(token)
            .is_some_and(|entry| entry.registration_id == registration_id)
        {
            entries.remove(token);
        }
        return Ok((PendingApprovalResolution::ReceiverClosed, None));
    }

    let Some(pending) = pending_approvals.remove(token) else {
        return Ok((PendingApprovalResolution::NotFound, None));
    };
    drop(pending_approvals);
    if pending
        .audit
        .deliver_reply(
            pending.sender,
            reply.actor,
            reply.response.clone(),
            serde_json::json!({"destination":destination}),
        )
        .await?
    {
        Ok((PendingApprovalResolution::Resolved, Some(pending.tool_name)))
    } else {
        pending
            .audit
            .record_rejection(
                "receiver_closed",
                serde_json::json!({"destination": destination}),
            )
            .await?;
        Ok((PendingApprovalResolution::ReceiverClosed, None))
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

/// Fail with the vendor's status and body when an HTTP response is not a
/// success, otherwise hand the response back so the caller can read it.
///
/// This is the one shape twenty-nine hand-written checks shared: a status
/// test, the status captured, the body read best-effort, and
/// `"<what> failed (<status>): <body>"`. Keeping the message byte-identical
/// means callers migrate without changing what an operator sees in a log.
/// Sites that need something else, such as a typed error, a sanitized body, a
/// status-specific retry, or a body that must stay unread so it can be
/// streamed — keep their own check on purpose.
#[cfg(any(
    feature = "channel-dingtalk",
    feature = "channel-discord",
    feature = "channel-line",
    feature = "channel-mochat",
    feature = "channel-qq",
    feature = "channel-twitter",
    feature = "channel-wechat",
    feature = "channel-wecom"
))]
pub(crate) async fn ensure_success(
    resp: reqwest::Response,
    what: &str,
) -> anyhow::Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let err = resp.text().await.unwrap_or_default();
    anyhow::bail!("{what} failed ({status}): {err}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn approval_reply_delivers_its_original_durable_identity_receipt() {
        use approval_audit_test_support::ApprovalRecorder;
        use zeroclaw_api::channel::{ApprovalSource, ChannelApprovalResponse};
        use zeroclaw_api::session_events::EventActor;

        struct ReplyChannel;
        impl zeroclaw_api::attribution::Attributable for ReplyChannel {
            fn role(&self) -> zeroclaw_api::attribution::Role {
                zeroclaw_api::attribution::Role::Channel(
                    zeroclaw_api::attribution::ChannelKind::Matrix,
                )
            }
            fn alias(&self) -> &str {
                "reviewer"
            }
        }

        for fail in [false, true] {
            let recorder = Arc::new(ApprovalRecorder::default());
            recorder
                .fail
                .store(fail, std::sync::atomic::Ordering::SeqCst);
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let pending = tokio::sync::Mutex::new(HashMap::from([(
                "token".into(),
                PendingApproval {
                    registration_id: uuid::Uuid::new_v4(),
                    audit: recorder.capture().await,
                    sender,
                    destination: "room".into(),
                    tool_name: "shell".into(),
                },
            )]));
            let result = resolve_pending_approval(
                &pending,
                "token",
                ApprovalReply::authenticated(
                    ChannelApprovalResponse::AlwaysApprove,
                    &ReplyChannel,
                    "@reviewer:example.invalid",
                ),
                true,
                "room",
            )
            .await;
            assert!(pending.lock().await.is_empty());
            if fail {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("test_approval_append_failed")
                );
                assert!(recorder.replies.lock().unwrap().is_empty());
                let waiter_error = receiver.await.unwrap().unwrap_err();
                assert!(zeroclaw_api::session_events::capture::is_capture_stopped(
                    &waiter_error
                ));
                assert!(format!("{waiter_error:#}").contains("test_approval_append_failed"));
            } else {
                assert_eq!(result.unwrap(), PendingApprovalResolution::Resolved);
                let delivered = receiver.await.unwrap().unwrap();
                assert_eq!(delivered.response, ChannelApprovalResponse::AlwaysApprove);
                assert_eq!(delivered.source, ApprovalSource::Operator);
                assert!(delivered.reply_event_id.is_some());
                let replies = recorder.replies.lock().unwrap();
                assert_eq!(replies.len(), 1);
                assert_eq!(
                    replies[0].0,
                    EventActor::ChannelResponder {
                        channel: "matrix.reviewer".into(),
                        responder_id: "@reviewer:example.invalid".into(),
                    }
                );
                assert_eq!(replies[0].1, ChannelApprovalResponse::AlwaysApprove);
            }
        }
    }

    #[tokio::test]
    async fn approval_audit_is_owned_by_registration_even_when_tokens_match() {
        use approval_audit_test_support::ApprovalRecorder;
        use zeroclaw_api::channel::ChannelApprovalResponse;

        let first = Arc::new(ApprovalRecorder::default());
        let second = Arc::new(ApprovalRecorder::default());
        let (first_tx, first_rx) = tokio::sync::oneshot::channel();
        let (second_tx, second_rx) = tokio::sync::oneshot::channel();
        let first_pending = tokio::sync::Mutex::new(HashMap::from([(
            "same-token".into(),
            PendingApproval {
                registration_id: uuid::Uuid::new_v4(),
                audit: first.capture().await,
                sender: first_tx,
                destination: "first-room".into(),
                tool_name: "shell".into(),
            },
        )]));
        let second_pending = tokio::sync::Mutex::new(HashMap::from([(
            "same-token".into(),
            PendingApproval {
                registration_id: uuid::Uuid::new_v4(),
                audit: second.capture().await,
                sender: second_tx,
                destination: "second-room".into(),
                tool_name: "shell".into(),
            },
        )]));

        assert_eq!(
            resolve_pending_approval(
                &first_pending,
                "same-token",
                ChannelApprovalResponse::Approve,
                false,
                "first-room"
            )
            .await
            .unwrap(),
            PendingApprovalResolution::Rejected
        );
        assert_eq!(first.records.lock().unwrap().len(), 1);
        assert!(second.records.lock().unwrap().is_empty());
        assert_eq!(
            resolve_pending_approval(
                &first_pending,
                "same-token",
                ChannelApprovalResponse::Deny,
                true,
                "first-room"
            )
            .await
            .unwrap(),
            PendingApprovalResolution::Resolved
        );
        assert_eq!(
            first_rx.await.unwrap().unwrap().response,
            ChannelApprovalResponse::Deny
        );
        assert_eq!(Arc::strong_count(&first), 1);

        drop(second_rx);
        assert_eq!(
            resolve_pending_approval(
                &second_pending,
                "same-token",
                ChannelApprovalResponse::Approve,
                true,
                "second-room"
            )
            .await
            .unwrap(),
            PendingApprovalResolution::ReceiverClosed
        );
        assert_eq!(second.records.lock().unwrap()[0].0, "receiver_closed");
        assert_eq!(Arc::strong_count(&second), 1);
    }

    #[tokio::test]
    async fn approval_audit_failure_reaches_resolver_without_resolving_request() {
        use approval_audit_test_support::ApprovalRecorder;
        use zeroclaw_api::channel::ChannelApprovalResponse;

        let recorder = Arc::new(ApprovalRecorder::default());
        recorder
            .fail
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let (sender, mut receiver) = tokio::sync::oneshot::channel();
        let pending = tokio::sync::Mutex::new(HashMap::from([(
            "token".into(),
            PendingApproval {
                registration_id: uuid::Uuid::new_v4(),
                audit: recorder.capture().await,
                sender,
                destination: "room".into(),
                tool_name: "shell".into(),
            },
        )]));
        let error = resolve_pending_approval(
            &pending,
            "token",
            ChannelApprovalResponse::Approve,
            false,
            "room",
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "test_approval_append_failed");
        assert!(pending.lock().await.contains_key("token"));
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        recorder
            .fail
            .store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            resolve_pending_approval(
                &pending,
                "token",
                ChannelApprovalResponse::Approve,
                false,
                "room"
            )
            .await
            .unwrap(),
            PendingApprovalResolution::Rejected
        );
        assert_eq!(recorder.records.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn pending_guard_releases_audit_after_contended_cancellation() {
        use approval_audit_test_support::ApprovalRecorder;
        let recorder = Arc::new(ApprovalRecorder::default());
        let (sender, _receiver) = tokio::sync::oneshot::channel();
        let pending = Arc::new(tokio::sync::Mutex::new(HashMap::from([(
            "token".into(),
            PendingApproval {
                registration_id: uuid::Uuid::new_v4(),
                audit: recorder.capture().await,
                sender,
                destination: "room".into(),
                tool_name: "shell".into(),
            },
        )])));
        let registration_id = pending.lock().await["token"].registration_id;
        let guard =
            PendingApprovalGuard::new(Arc::clone(&pending), "token".into(), registration_id);
        let lock = pending.lock().await;
        drop(guard);
        assert_eq!(Arc::strong_count(&recorder), 2);
        drop(lock);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if pending.lock().await.is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(Arc::strong_count(&recorder), 1);
    }

    #[cfg(feature = "channel-qq")]
    #[tokio::test]
    async fn ensure_success_passes_a_success_through_and_reports_status_and_body_otherwise() {
        let (url, server) = spawn_raw_http_response(
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
            false,
        )
        .await;
        let resp = reqwest::get(&url).await.unwrap();
        let resp = ensure_success(resp, "QQ sendMessage").await.unwrap();
        assert_eq!(resp.text().await.unwrap(), "ok");
        server.await.unwrap();

        let (url, server) = spawn_raw_http_response(
            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 5\r\n\r\nboom!".to_vec(),
            false,
        )
        .await;
        let resp = reqwest::get(&url).await.unwrap();
        let err = ensure_success(resp, "QQ sendMessage").await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "QQ sendMessage failed (500 Internal Server Error): boom!"
        );
        server.await.unwrap();
    }

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

    /// A reply opening with an ElevenLabs v3 expressive audio tag is prose
    /// written for speech, so it must reach TTS rather than be filtered out
    /// as machine output.
    #[test]
    fn voice_reply_accepts_leading_audio_tags() {
        for content in [
            "[dramatic] Signori, si alza il sipario sulla serata.",
            "[exhales] Ascoltate, ascoltate... e l'aria della sera.",
            "[very excited] Ho finito di preparare il tuo riepilogo!",
            "[pause 2s] Adesso arriva la parte piu interessante del racconto.",
            "[strong French accent] Bonjour, comment allez-vous aujourd'hui?",
            "[whispers][slowly] Ascoltate bene quello che sto per dire.",
        ] {
            assert_eq!(voice_reply_skip_reason(content), None, "input: {content}");
        }
    }

    /// The control case from the report: prose with no leading bracket was
    /// always voiced and must stay that way.
    #[test]
    fn voice_reply_accepts_plain_prose() {
        assert_eq!(
            voice_reply_skip_reason("Si apra il sipario, si accordi l'orchestra!"),
            None
        );
    }

    /// The bracket clause still has to reject genuine machine output.
    #[test]
    fn voice_reply_rejects_bracketed_machine_output() {
        for (content, expected) in [
            (
                "[{\"a\":1},{\"b\":2}] ecco il meteo di oggi per Roma.",
                Some("json_array"),
            ),
            (
                "[\"alpha\",\"beta\",\"gamma\",\"delta\",\"epsilon\"]",
                Some("json_array"),
            ),
            (
                "[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]",
                Some("json_array"),
            ),
            (
                "[] plus filler text to clear the length floor here",
                Some("json_array"),
            ),
            (
                "[IMAGE:/home/pi/chart.png] Ecco il grafico richiesto.",
                None,
            ),
            (
                "[VOICE:/tmp/zeroclaw/out.ogg] Nota vocale registrata.",
                None,
            ),
            ("[DOCUMENT:/a.pdf] Ecco il file del report richiesto.", None),
            (
                "[Guida](https://example.com/docs) ecco il link utile.",
                Some("markdown_link"),
            ),
            (
                "[unclosed tag and a sentence that never closes it",
                Some("unclosed_bracket"),
            ),
            (
                "[didascalia molto lunga che sfora il limite dei tag] ok",
                Some("bracketed_prefix"),
            ),
        ] {
            assert_eq!(
                voice_reply_skip_reason(content),
                expected,
                "input: {content}"
            );
        }
    }

    /// The clauses that predate the audio-tag fix keep their behavior.
    #[test]
    fn voice_reply_rejects_non_bracket_machine_output() {
        for (content, expected) in [
            (
                "{\"ok\":true,\"result\":{\"message_id\":123456}}",
                "json_object",
            ),
            (
                "https://example.com/a/very/long/path/that/clears",
                "url_prefix",
            ),
            (
                "Error: the provider refused the request again.",
                "error_prefix",
            ),
            (
                "Ecco:\n```bash\nls -la\n``` e poi fammi sapere tutto.",
                "code_fence",
            ),
            (
                "Ho ricevuto un tool_call malformato, riprovo.",
                "tool_call_marker",
            ),
            (
                "Meteo da wttr.in: Roma 21 gradi, cielo sereno.",
                "weather_tool_output",
            ),
        ] {
            assert_eq!(
                voice_reply_skip_reason(content),
                Some(expected),
                "input: {content}"
            );
        }
    }

    /// The length floor is measured on the full reply, tag included, so a
    /// leading tag can never shorten a reply into rejection.
    #[test]
    fn voice_reply_length_floor_is_measured_on_the_full_reply() {
        assert_eq!(voice_reply_skip_reason(&"x".repeat(41)), None);
        assert_eq!(voice_reply_skip_reason(&"x".repeat(40)), Some("too_short"));
        assert_eq!(
            voice_reply_skip_reason(&format!("[whispers]{}", "x".repeat(31))),
            None
        );
        assert_eq!(voice_reply_skip_reason("[laughs]"), Some("too_short"));
        assert_eq!(
            voice_reply_skip_reason("[dramatic] Ciao!"),
            Some("too_short")
        );
    }

    /// Pins the classifier so expressive tags remain distinct from ordinary
    /// bracketed text.
    #[test]
    fn leading_bracket_classifier_distinguishes_tags_from_text() {
        assert_eq!(leading_bracket_skip_reason("[whispers] rest"), None);
        assert_eq!(leading_bracket_skip_reason("[IMAGE:/x] rest"), None);
        assert_eq!(leading_bracket_skip_reason("no bracket here"), None);
    }

    #[test]
    fn format_location_content_omits_empty_name() {
        assert_eq!(
            format_location_content(40.7128, -74.0060, Some("NYC")),
            "Location: 40.712800, -74.006000 — NYC"
        );
        assert_eq!(
            format_location_content(40.7128, -74.0060, Some("")),
            "Location: 40.712800, -74.006000"
        );
        assert_eq!(
            format_location_content(40.7128, -74.0060, None),
            "Location: 40.712800, -74.006000"
        );
    }

    #[test]
    fn new_approval_token_is_6_char_alphanumeric() {
        let token = super::new_approval_token();
        assert_eq!(token.len(), 6);
        assert!(token.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[tokio::test]
    async fn pending_approval_guard_normal_cleanup_and_disarm_preserve_owner_resolution() {
        let pending = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let registration_id = uuid::Uuid::new_v4();
        pending
            .lock()
            .await
            .insert("normal".to_string(), registration_id);
        let mut guard =
            PendingApprovalGuard::new(Arc::clone(&pending), "normal".to_string(), registration_id);
        guard.remove().await;
        assert!(!pending.lock().await.contains_key("normal"));
        drop(guard);

        pending
            .lock()
            .await
            .insert("resolved".to_string(), registration_id);
        let mut guard = PendingApprovalGuard::new(
            Arc::clone(&pending),
            "resolved".to_string(),
            registration_id,
        );
        guard.disarm();
        drop(guard);
        assert!(pending.lock().await.contains_key("resolved"));
    }

    #[tokio::test]
    async fn pending_approval_guard_removes_entry_after_cancellation() {
        let pending = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let registration_id = uuid::Uuid::new_v4();
        pending
            .lock()
            .await
            .insert("cancelled".to_string(), registration_id);
        let (armed_tx, armed_rx) = tokio::sync::oneshot::channel();
        let task_pending = Arc::clone(&pending);
        let task = zeroclaw_spawn::spawn!(async move {
            let _guard =
                PendingApprovalGuard::new(task_pending, "cancelled".to_string(), registration_id);
            let _ = armed_tx.send(());
            std::future::pending::<()>().await;
        });
        armed_rx.await.unwrap();
        task.abort();
        let _ = task.await;

        assert!(!pending.lock().await.contains_key("cancelled"));
    }

    #[tokio::test]
    async fn pending_approval_guard_remove_takes_entry_and_disarms_drop() {
        let pending = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let registration = uuid::Uuid::new_v4();
        pending
            .lock()
            .await
            .insert("removed".to_string(), registration);
        let mut guard =
            PendingApprovalGuard::new(Arc::clone(&pending), "removed".to_string(), registration);

        guard.remove().await;
        assert!(!pending.lock().await.contains_key("removed"));

        // A still-armed guard would remove this re-registered entry on drop.
        pending
            .lock()
            .await
            .insert("removed".to_string(), registration);
        drop(guard);

        assert!(pending.lock().await.contains_key("removed"));
    }

    #[tokio::test]
    async fn pending_approval_guard_disarm_leaves_entry_on_drop() {
        let pending = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let registration = uuid::Uuid::new_v4();
        pending
            .lock()
            .await
            .insert("claimed".to_string(), registration);
        let mut guard =
            PendingApprovalGuard::new(Arc::clone(&pending), "claimed".to_string(), registration);

        guard.disarm();
        drop(guard);

        assert!(pending.lock().await.contains_key("claimed"));
    }

    #[tokio::test]
    async fn stale_pending_guard_cannot_remove_a_new_registration() {
        let old = uuid::Uuid::new_v4();
        let new = uuid::Uuid::new_v4();
        let pending = Arc::new(tokio::sync::Mutex::new(HashMap::from([(
            "token".into(),
            old,
        )])));
        let mut stale = PendingApprovalGuard::new(Arc::clone(&pending), "token".into(), old);
        pending.lock().await.insert("token".into(), new);
        stale.remove().await;
        assert_eq!(pending.lock().await["token"], new);
        let stale = PendingApprovalGuard::new(Arc::clone(&pending), "token".into(), old);
        drop(stale);
        assert_eq!(pending.lock().await["token"], new);
    }

    #[tokio::test]
    async fn duplicate_pending_registration_preserves_the_original_waiter() {
        use zeroclaw_api::channel::ChannelApprovalResponse;
        let pending = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let mut guard = register_pending_approval(
            Arc::clone(&pending),
            "token".into(),
            sender,
            "room".into(),
            "shell".into(),
        )
        .await
        .unwrap();
        let (duplicate, _duplicate_receiver) = tokio::sync::oneshot::channel();
        let result = register_pending_approval(
            Arc::clone(&pending),
            "token".into(),
            duplicate,
            "other-room".into(),
            "shell".into(),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(
            resolve_pending_approval(
                &pending,
                "token",
                ChannelApprovalResponse::Deny,
                true,
                "room"
            )
            .await
            .unwrap(),
            PendingApprovalResolution::Resolved
        );
        assert_eq!(
            receiver.await.unwrap().unwrap().response,
            ChannelApprovalResponse::Deny
        );
        guard.disarm();
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
        let prompt = super::build_yesno_approval_prompt(token, "shell", "ls -la", None);
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
    fn approval_prompt_shows_batch_position_when_batch_has_several_calls() {
        // Back-to-back cards from one message are indistinguishable before the
        // operator taps, so a multi-call batch must say which call it is.
        let prompt = super::build_yesno_approval_prompt("ab12cd", "shell", "ls -la", Some((2, 3)));
        assert!(
            prompt.contains('2') && prompt.contains('3'),
            "prompt should carry the batch counter; got {prompt:?}"
        );
        // The counter belongs above the tool line so it is read first.
        let counter_at = prompt
            .find('2')
            .expect("counter should be present in the prompt");
        let tool_at = prompt
            .find("shell")
            .expect("tool name should be present in the prompt");
        assert!(
            counter_at < tool_at,
            "counter should precede the tool line; got {prompt:?}"
        );
    }

    #[test]
    fn approval_prompt_omits_position_for_a_single_call_batch() {
        // `1 of 1` tells the operator nothing, and every adapter would
        // otherwise have to special-case it.
        let with_single = super::build_yesno_approval_prompt("ab12cd", "shell", "x", Some((1, 1)));
        let without = super::build_yesno_approval_prompt("ab12cd", "shell", "x", None);
        assert_eq!(
            with_single, without,
            "a one-call batch should render exactly as an unpositioned prompt"
        );
    }

    #[test]
    fn position_counter_reports_raw_batch_position_not_approval_count() {
        use zeroclaw_api::channel::{ApprovalPosition, ChannelApprovalRequest};

        // The batch is three calls and only the second needs approval. The
        // card must read "2 of 3" — the model-issued position — rather than
        // "1 of 1", which would be the approval-required count. Computing the
        // latter would require the approval set before the first card renders.
        let request = ChannelApprovalRequest {
            tool_name: "stake_tx_build".to_string(),
            arguments_summary: "action: deactivate".to_string(),
            raw_arguments: None,
            position: Some(ApprovalPosition { index: 2, total: 3 }),
        };

        assert_eq!(request.position_counter(), Some((2, 3)));
    }

    #[test]
    fn position_counter_is_absent_without_a_position() {
        use zeroclaw_api::channel::ChannelApprovalRequest;

        let request = ChannelApprovalRequest {
            tool_name: "shell".to_string(),
            arguments_summary: "ls -la".to_string(),
            raw_arguments: None,
            position: None,
        };

        assert_eq!(request.position_counter(), None);
    }

    #[test]
    fn approve_deny_approval_prompt_matches_matrix_own_parser_keywords() {
        // Same desync guard as above, for Matrix's `approve`/`deny`/`always`
        // reply shape (Matrix uses its own parser, not
        // `parse_approval_reply`, but the keyword contract is identical).
        let token = "AB12CD34";
        let prompt = super::build_approve_deny_approval_prompt(token, "shell", "ls -la", None);
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
                    ("target", "TARGET"),
                    ("caller", "CALLER"),
                    ("yes_command", "TKN yes"),
                    ("no_command", "TKN no"),
                    ("approve_command", "TKN approve"),
                    ("deny_command", "TKN deny"),
                    ("always_command", "TKN always"),
                    ("index", "1"),
                    ("total", "2"),
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
                registration_id: uuid::Uuid::new_v4(),
                audit: ApprovalAudit::current(),
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
        .await
        .unwrap();
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
        .await
        .unwrap();
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
        .await
        .unwrap();
        assert_eq!(resolved, PendingApprovalResolution::Resolved);
        assert!(resolved.suppresses_message());
        assert_eq!(
            rx.await.unwrap().unwrap().response,
            ChannelApprovalResponse::AlwaysApprove
        );
        assert!(pending.lock().await.is_empty());

        let not_found = resolve_pending_approval(
            &pending,
            "missing-approval-id",
            ChannelApprovalResponse::Approve,
            true,
            "room-a",
        )
        .await
        .unwrap();
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
                registration_id: uuid::Uuid::new_v4(),
                audit: ApprovalAudit::current(),
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
        .await
        .unwrap();
        assert_eq!(resolution, PendingApprovalResolution::ReceiverClosed);
        assert!(
            resolution.suppresses_message(),
            "a consumed approval must not fall through after its receiver closes"
        );
        assert!(pending.lock().await.is_empty());
    }
}
