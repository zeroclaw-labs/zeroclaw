use async_trait::async_trait;
use base64::Engine as _;
use futures_util::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_api::media::MediaAttachment;

const GROUP_TARGET_PREFIX: &str = "group:";
const SIGNAL_INBOUND_SUBDIR: &str = "signal_inbound";

#[derive(Debug, Clone, PartialEq, Eq)]
enum RecipientTarget {
    Direct(String),
    Group(String),
}

/// Signal channel using signal-cli daemon's native JSON-RPC + SSE API.
///
/// Connects to a running `signal-cli daemon --http <host:port>`.
/// Listens via SSE at `/api/v1/events` and sends via JSON-RPC at
/// `/api/v1/rpc`.
#[derive(Clone)]
pub struct SignalChannel {
    http_url: String,
    account: String,
    group_id: Option<String>,
    allowed_from: Vec<String>,
    ignore_attachments: bool,
    ignore_stories: bool,
    /// Per-channel proxy URL override.
    proxy_url: Option<String>,
    /// Workspace directory used for persisting inbound attachments and
    /// resolving `/workspace/` prefixes on outbound `[IMAGE:...]` markers.
    workspace_dir: Option<PathBuf>,
}

// ── signal-cli SSE event JSON shapes ────────────────────────────

#[derive(Debug, Deserialize)]
struct SseEnvelope {
    #[serde(default)]
    envelope: Option<Envelope>,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    source: Option<String>,
    #[serde(rename = "sourceNumber", default)]
    source_number: Option<String>,
    #[serde(rename = "dataMessage", default)]
    data_message: Option<DataMessage>,
    #[serde(rename = "storyMessage", default)]
    story_message: Option<serde_json::Value>,
    #[serde(default)]
    timestamp: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DataMessage {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    timestamp: Option<u64>,
    #[serde(rename = "groupInfo", default)]
    group_info: Option<GroupInfo>,
    #[serde(default)]
    attachments: Option<Vec<SignalAttachment>>,
}

#[derive(Debug, Deserialize)]
struct GroupInfo {
    #[serde(rename = "groupId", default)]
    group_id: Option<String>,
}

/// Single attachment entry inside a `dataMessage.attachments[]`.
///
/// Mirrors signal-cli's `JsonAttachment` shape. All non-`contentType` fields
/// are nullable. The `id` is opaque and used as the parameter to
/// `getAttachment` JSON-RPC to fetch the bytes.
#[derive(Debug, Deserialize, Clone)]
struct SignalAttachment {
    #[serde(rename = "contentType", default)]
    content_type: Option<String>,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    id: Option<String>,
}

// ── markdown → signal textStyles ─────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SignalStyle {
    Bold,
    Italic,
    Strikethrough,
    Spoiler,
    Monospace,
}

impl SignalStyle {
    fn as_str(self) -> &'static str {
        match self {
            Self::Bold => "BOLD",
            Self::Italic => "ITALIC",
            Self::Strikethrough => "STRIKETHROUGH",
            Self::Spoiler => "SPOILER",
            Self::Monospace => "MONOSPACE",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DelimKind {
    StarStar,
    Tilde,
    Pipe,
    Star,
    Underscore,
    Backtick,
}

#[derive(Clone, Copy, Debug)]
struct Marker {
    byte_pos: usize,
    delim_len: usize,
    delim_kind: DelimKind,
    style: SignalStyle,
}

/// Scan input for markdown delimiter positions and `\` escape positions.
///
/// Returns `(markers, escape_positions)` where `escape_positions[i]` is true
/// when byte `i` is the leading `\` of an `\<delim>` escape sequence (the
/// backslash should be dropped, the following char emitted literally).
fn scan_markers(input: &str) -> (Vec<Marker>, Vec<bool>) {
    let bytes = input.as_bytes();
    let mut markers: Vec<Marker> = Vec::new();
    let mut escapes = vec![false; bytes.len()];
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];

        // Backslash escape for delimiter chars or backslash itself.
        if b == b'\\'
            && let Some(&next) = bytes.get(i + 1)
            && matches!(next, b'*' | b'_' | b'~' | b'|' | b'`' | b'\\')
        {
            escapes[i] = true;
            i += 2; // skip both bytes; the escaped char is emitted literally on a later pass
            continue;
        }

        // Match delimiters (longest first for two-char ones).
        let (kind, style, len) = if b == b'*' && bytes.get(i + 1) == Some(&b'*') {
            (DelimKind::StarStar, SignalStyle::Bold, 2)
        } else if b == b'~' && bytes.get(i + 1) == Some(&b'~') {
            (DelimKind::Tilde, SignalStyle::Strikethrough, 2)
        } else if b == b'|' && bytes.get(i + 1) == Some(&b'|') {
            (DelimKind::Pipe, SignalStyle::Spoiler, 2)
        } else if b == b'*' {
            (DelimKind::Star, SignalStyle::Italic, 1)
        } else if b == b'_' {
            (DelimKind::Underscore, SignalStyle::Italic, 1)
        } else if b == b'`' {
            (DelimKind::Backtick, SignalStyle::Monospace, 1)
        } else {
            // Not a delimiter — advance by one char (UTF-8 safe).
            let ch_len = input[i..].chars().next().map_or(1, |c| c.len_utf8());
            i += ch_len;
            continue;
        };

        markers.push(Marker {
            byte_pos: i,
            delim_len: len,
            delim_kind: kind,
            style,
        });
        i += len;
    }
    (markers, escapes)
}

/// Pair markers into open/close ranges using a stack with flanking heuristics.
///
/// Returned tuples are `(open_idx, close_idx, style)` indexing into `markers`.
fn pair_markers(markers: &[Marker], input: &str) -> Vec<(usize, usize, SignalStyle)> {
    let bytes = input.as_bytes();
    let mut paired: Vec<(usize, usize, SignalStyle)> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();

    for (k, m) in markers.iter().enumerate() {
        // Inside a backtick span, only another backtick is meaningful.
        let in_mono = stack
            .iter()
            .any(|&j| markers[j].delim_kind == DelimKind::Backtick);
        if in_mono && m.delim_kind != DelimKind::Backtick {
            continue;
        }

        let single = matches!(
            m.delim_kind,
            DelimKind::Star | DelimKind::Underscore | DelimKind::Backtick
        );
        let prev_byte = if m.byte_pos == 0 {
            None
        } else {
            bytes.get(m.byte_pos - 1).copied()
        };
        let next_byte = bytes.get(m.byte_pos + m.delim_len).copied();

        // Try close: matching delim_kind on top of stack + right-flanking ok.
        let top_match = stack
            .last()
            .copied()
            .filter(|&j| markers[j].delim_kind == m.delim_kind);
        if let Some(top_idx) = top_match {
            let prev_ok = matches!(prev_byte, Some(b) if !b.is_ascii_whitespace());
            let next_ok = if single {
                !matches!(next_byte, Some(b) if b.is_ascii_alphanumeric())
            } else {
                true
            };
            if prev_ok && next_ok {
                stack.pop();
                paired.push((top_idx, k, markers[top_idx].style));
                continue;
            }
        }

        // Try open: left-flanking + non-whitespace right.
        let next_ok = matches!(next_byte, Some(b) if !b.is_ascii_whitespace());
        let prev_ok = if single {
            !matches!(prev_byte, Some(b) if b.is_ascii_alphanumeric())
        } else {
            true
        };
        if prev_ok && next_ok {
            stack.push(k);
        }
    }

    paired
}

/// Walk `input`, emit plain text, and resolve UTF-16 offsets for each paired
/// marker pair. Skipped: paired marker bytes and `\` escape leading bytes.
fn emit_with_offsets(
    input: &str,
    markers: &[Marker],
    paired: &[(usize, usize, SignalStyle)],
    escapes: &[bool],
) -> (String, Vec<String>) {
    use std::collections::{HashMap, HashSet};

    let bytes = input.as_bytes();

    let mut paired_set: HashSet<usize> = HashSet::new();
    for (a, b, _) in paired {
        paired_set.insert(*a);
        paired_set.insert(*b);
    }

    // byte_pos -> exclusive end byte_pos for paired marker delimiters.
    let mut skip_at: HashMap<usize, usize> = HashMap::new();
    // byte_pos -> list of marker indices starting at that byte.
    let mut marker_pos_to_idx: HashMap<usize, Vec<usize>> = HashMap::new();
    for (k, m) in markers.iter().enumerate() {
        marker_pos_to_idx.entry(m.byte_pos).or_default().push(k);
        if paired_set.contains(&k) {
            skip_at.insert(m.byte_pos, m.byte_pos + m.delim_len);
        }
    }

    let mut marker_utf16: HashMap<usize, usize> = HashMap::new();
    let mut out = String::new();
    let mut utf16: usize = 0;
    let mut i = 0;
    while i < bytes.len() {
        if let Some(mks) = marker_pos_to_idx.get(&i) {
            for &k in mks {
                if paired_set.contains(&k) {
                    marker_utf16.insert(k, utf16);
                }
            }
        }
        if let Some(&end) = skip_at.get(&i) {
            i = end;
            continue;
        }
        if escapes[i] {
            // Drop the leading backslash; the escaped char is the next char.
            i += 1;
            continue;
        }
        let ch = input[i..].chars().next().expect("char boundary");
        out.push(ch);
        utf16 += ch.len_utf16();
        i += ch.len_utf8();
    }
    if let Some(mks) = marker_pos_to_idx.get(&i) {
        for &k in mks {
            if paired_set.contains(&k) {
                marker_utf16.insert(k, utf16);
            }
        }
    }

    let mut style_strings: Vec<String> = Vec::new();
    for (open_k, close_k, style) in paired {
        let start = marker_utf16[open_k];
        let end = marker_utf16[close_k];
        if end > start {
            style_strings.push(format!("{}:{}:{}", start, end - start, style.as_str()));
        }
    }
    style_strings.sort();

    (out, style_strings)
}

/// Convert markdown-flavored text into plain text plus signal-cli textStyle
/// range strings (`"start:length:STYLE"`, UTF-16 offsets).
///
/// Supported syntax: `**bold**`, `*italic*` / `_italic_`, `~~strikethrough~~`,
/// `||spoiler||`, `` `monospace` ``. Use `\` to escape a delimiter literally
/// (e.g. `\*` → literal `*`). Inside a backtick span all other delimiters
/// are taken literally (code semantics).
pub(crate) fn markdown_to_signal_text(input: &str) -> (String, Vec<String>) {
    let (markers, escapes) = scan_markers(input);
    let paired = pair_markers(&markers, input);
    emit_with_offsets(input, &markers, &paired, &escapes)
}

/// Encode a media attachment as an RFC 2397 data URI for signal-cli.
pub(crate) fn encode_attachment_data_uri(att: &MediaAttachment) -> String {
    let mime = att
        .mime_type
        .as_deref()
        .unwrap_or("application/octet-stream");
    let b64 = base64::engine::general_purpose::STANDARD.encode(&att.data);
    format!("data:{};filename={};base64,{}", mime, att.file_name, b64)
}

/// Find the index of the `]` that closes a bracket opened just before `s`
/// (the caller has already consumed the leading `[`). Nested `[...]` pairs
/// are skipped so marker contents containing `[` won't close early.
fn find_matching_close_bracket(s: &str) -> Option<usize> {
    let mut depth = 1usize;
    for (i, c) in s.char_indices() {
        match c {
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

/// Strip `[KIND:target]` media markers from `text`, returning cleaned text
/// and the ordered list of targets. Recognised kinds: `IMAGE`, `PHOTO`,
/// `VIDEO`, `AUDIO`, `VOICE`, `DOCUMENT`, `FILE`. Unknown brackets pass
/// through verbatim.
pub(crate) fn parse_media_markers(text: &str) -> (String, Vec<String>) {
    let mut cleaned = String::with_capacity(text.len());
    let mut targets: Vec<String> = Vec::new();
    let mut cursor = 0;

    while cursor < text.len() {
        let Some(open_rel) = text[cursor..].find('[') else {
            cleaned.push_str(&text[cursor..]);
            break;
        };
        let open = cursor + open_rel;
        cleaned.push_str(&text[cursor..open]);

        let Some(close_rel) = find_matching_close_bracket(&text[open + 1..]) else {
            cleaned.push_str(&text[open..]);
            break;
        };
        let close = open + 1 + close_rel;
        let marker = &text[open + 1..close];

        let target = marker.split_once(':').and_then(|(kind, rest)| {
            let k = kind.trim().to_ascii_uppercase();
            let is_media = matches!(
                k.as_str(),
                "IMAGE" | "PHOTO" | "VIDEO" | "AUDIO" | "VOICE" | "DOCUMENT" | "FILE"
            );
            if !is_media {
                return None;
            }
            let t = rest.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        });

        if let Some(t) = target {
            targets.push(t);
        } else {
            cleaned.push_str(&text[open..=close]);
        }
        cursor = close + 1;
    }

    // Collapse runs of whitespace left by removed markers into single spaces,
    // and trim ends, so removing a marker doesn't leave visible gaps.
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    (collapsed, targets)
}

/// Infer a file extension from a MIME type for inbound attachment storage.
fn extension_for_mime(mime: &str) -> Option<&'static str> {
    let lower = mime
        .split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase();
    match lower.as_str() {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/heic" | "image/heif" => Some("heic"),
        "video/mp4" => Some("mp4"),
        "video/quicktime" => Some("mov"),
        "video/webm" => Some("webm"),
        "audio/ogg" => Some("ogg"),
        "audio/mpeg" => Some("mp3"),
        "audio/mp4" => Some("m4a"),
        "audio/webm" => Some("webm"),
        "audio/wav" | "audio/x-wav" => Some("wav"),
        "application/pdf" => Some("pdf"),
        _ => None,
    }
}

/// True when a path extension or MIME type identifies an image attachment.
fn is_image_attachment(path: &Path, mime: Option<&str>) -> bool {
    if let Some(m) = mime
        && m.to_ascii_lowercase().starts_with("image/")
    {
        return true;
    }
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "heic" | "heif")
    )
}

/// Keep only filename-safe characters. signal-cli attachment ids are opaque
/// and could in principle contain characters we'd rather not put into a path.
fn sanitize_id_for_filename(id: &str) -> String {
    let filtered: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    if filtered.is_empty() {
        "attachment".to_string()
    } else {
        filtered
    }
}

/// Build the inbound-marker text for an attachment saved on disk.
fn signal_inbound_marker(path: &Path, filename: &str, mime: Option<&str>) -> String {
    if is_image_attachment(path, mime) {
        format!("[IMAGE:{}]", path.display())
    } else {
        format!("[Document: {}] {}", filename, path.display())
    }
}

#[derive(Debug, Clone)]
struct AttachmentRef {
    id: String,
    filename: Option<String>,
    content_type: Option<String>,
}

impl SignalChannel {
    pub fn new(
        http_url: String,
        account: String,
        group_id: Option<String>,
        allowed_from: Vec<String>,
        ignore_attachments: bool,
        ignore_stories: bool,
    ) -> Self {
        let http_url = http_url.trim_end_matches('/').to_string();
        Self {
            http_url,
            account,
            group_id,
            allowed_from,
            ignore_attachments,
            ignore_stories,
            proxy_url: None,
            workspace_dir: None,
        }
    }

    /// Set a per-channel proxy URL that overrides the global proxy config.
    pub fn with_proxy_url(mut self, proxy_url: Option<String>) -> Self {
        self.proxy_url = proxy_url;
        self
    }

    /// Configure workspace directory for persisting inbound attachments and
    /// resolving `/workspace/` prefixes on outbound media markers.
    pub fn with_workspace_dir(mut self, dir: PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    fn http_client(&self) -> Client {
        let builder = Client::builder().connect_timeout(Duration::from_secs(10));
        let builder = zeroclaw_config::schema::apply_channel_proxy_to_builder(
            builder,
            "channel.signal",
            self.proxy_url.as_deref(),
        );
        builder.build().expect("Signal HTTP client should build")
    }

    /// Effective sender: prefer `sourceNumber` (E.164), fall back to `source`.
    fn sender(envelope: &Envelope) -> Option<String> {
        envelope
            .source_number
            .as_deref()
            .or(envelope.source.as_deref())
            .map(String::from)
    }

    fn is_sender_allowed(&self, sender: &str) -> bool {
        if self.allowed_from.iter().any(|u| u == "*") {
            return true;
        }
        self.allowed_from.iter().any(|u| u == sender)
    }

    fn is_e164(recipient: &str) -> bool {
        let Some(number) = recipient.strip_prefix('+') else {
            return false;
        };
        (2..=15).contains(&number.len()) && number.chars().all(|c| c.is_ascii_digit())
    }

    /// Check whether a string is a valid UUID (signal-cli uses these for
    /// privacy-enabled users who have opted out of sharing their phone number).
    fn is_uuid(s: &str) -> bool {
        Uuid::parse_str(s).is_ok()
    }

    fn parse_recipient_target(recipient: &str) -> RecipientTarget {
        if let Some(group_id) = recipient.strip_prefix(GROUP_TARGET_PREFIX) {
            return RecipientTarget::Group(group_id.to_string());
        }

        if Self::is_e164(recipient) || Self::is_uuid(recipient) {
            RecipientTarget::Direct(recipient.to_string())
        } else {
            RecipientTarget::Group(recipient.to_string())
        }
    }

    /// Check whether the message targets the configured group.
    /// If no `group_id` is configured (None), all DMs and groups are accepted.
    /// Use "dm" to filter DMs only.
    fn matches_group(&self, data_msg: &DataMessage) -> bool {
        let Some(ref expected) = self.group_id else {
            return true;
        };
        match data_msg
            .group_info
            .as_ref()
            .and_then(|g| g.group_id.as_deref())
        {
            Some(gid) => gid == expected.as_str(),
            None => expected.eq_ignore_ascii_case("dm"),
        }
    }

    /// Determine the send target: group id or the sender's number.
    fn reply_target(&self, data_msg: &DataMessage, sender: &str) -> String {
        if let Some(group_id) = data_msg
            .group_info
            .as_ref()
            .and_then(|g| g.group_id.as_deref())
        {
            format!("{GROUP_TARGET_PREFIX}{group_id}")
        } else {
            sender.to_string()
        }
    }

    /// Send a JSON-RPC request to signal-cli daemon.
    async fn rpc_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let url = format!("{}/api/v1/rpc", self.http_url);
        let id = Uuid::new_v4().to_string();

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id,
        });

        let resp = self
            .http_client()
            .post(&url)
            .timeout(Duration::from_secs(30))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        // 201 = success with no body (e.g. typing indicators)
        if resp.status().as_u16() == 201 {
            return Ok(None);
        }

        let text = resp.text().await?;
        if text.is_empty() {
            return Ok(None);
        }

        let parsed: serde_json::Value = serde_json::from_str(&text)?;
        if let Some(err) = parsed.get("error") {
            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown");
            anyhow::bail!("Signal RPC error {code}: {msg}");
        }

        Ok(parsed.get("result").cloned())
    }

    /// Resolve a `[KIND:target]` marker target into the string form signal-cli
    /// accepts inside the `attachments` array.
    ///
    /// - HTTP/HTTPS URLs are downloaded and encoded as RFC 2397 data URIs,
    ///   because signal-cli does not fetch URLs itself.
    /// - `/workspace/<rest>` paths are remapped to the configured workspace
    ///   directory, matching the convention used by the Telegram channel.
    /// - Other paths are returned verbatim, which signal-cli opens from the
    ///   host filesystem.
    async fn resolve_outbound_marker(&self, target: &str) -> anyhow::Result<String> {
        if target.starts_with("http://") || target.starts_with("https://") {
            let resp = self
                .http_client()
                .get(target)
                .timeout(Duration::from_secs(30))
                .send()
                .await?;
            if !resp.status().is_success() {
                anyhow::bail!("marker download returned status {}", resp.status());
            }
            let mime = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.split(';').next().unwrap_or(s).trim().to_string())
                .unwrap_or_else(|| "application/octet-stream".to_string());
            let filename = target
                .rsplit('/')
                .find(|seg| !seg.is_empty())
                .unwrap_or("file");
            let bytes = resp.bytes().await?;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            return Ok(format!(
                "data:{};filename={};base64,{}",
                mime, filename, b64
            ));
        }

        let remapped: PathBuf;
        let path_str = if let Some(rest) = target.strip_prefix("/workspace/")
            && let Some(ws) = self.workspace_dir.as_ref()
        {
            remapped = ws.join(rest);
            remapped.to_string_lossy().into_owned()
        } else {
            target.to_string()
        };

        let path = Path::new(&path_str);
        if !path.is_absolute() {
            anyhow::bail!("marker path must be absolute: {path_str}");
        }
        if !path.exists() {
            anyhow::bail!("marker path not found: {path_str}");
        }
        Ok(path_str)
    }

    /// Persist an inbound attachment to the workspace directory.
    ///
    /// Layout: `{workspace_dir}/signal_inbound/{sanitized_id}.{ext}`. The
    /// extension is taken from the incoming filename first, then the MIME
    /// type; if neither is known the file is written without an extension.
    /// Returns the absolute path on success or an error when the workspace
    /// is not configured / IO fails.
    async fn save_inbound_attachment(
        &self,
        id: &str,
        filename: &str,
        mime: Option<&str>,
        bytes: &[u8],
    ) -> anyhow::Result<PathBuf> {
        let ws = self
            .workspace_dir
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("workspace_dir not configured"))?;
        let dir = ws.join(SIGNAL_INBOUND_SUBDIR);
        tokio::fs::create_dir_all(&dir).await?;

        let ext = Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .or_else(|| mime.and_then(extension_for_mime).map(str::to_string));

        let stem = sanitize_id_for_filename(id);
        let out = match ext {
            Some(ext) => dir.join(format!("{stem}.{ext}")),
            None => dir.join(stem),
        };
        tokio::fs::write(&out, bytes).await?;
        Ok(out)
    }

    /// Build JSON-RPC `send` params for a single outbound message.
    ///
    /// Markdown in `message.content` is converted to plain text + textStyles
    /// ranges. Attachments on `message.attachments` are encoded as RFC 2397
    /// data URIs. Media markers (`[IMAGE:...]` etc.) are NOT expanded here —
    /// [`send`] runs marker resolution before calling this helper.
    fn build_send_params(&self, message: &SendMessage) -> serde_json::Value {
        let (text, text_styles) = markdown_to_signal_text(&message.content);

        let mut base = match Self::parse_recipient_target(&message.recipient) {
            RecipientTarget::Direct(number) => serde_json::json!({
                "recipient": [number],
                "account": &self.account,
            }),
            RecipientTarget::Group(group_id) => serde_json::json!({
                "groupId": group_id,
                "account": &self.account,
            }),
        };

        let obj = base
            .as_object_mut()
            .expect("send params constructed as object");
        if !text.is_empty() {
            obj.insert("message".into(), serde_json::Value::String(text));
        }
        if !text_styles.is_empty() {
            obj.insert("textStyles".into(), serde_json::json!(text_styles));
        }
        if !message.attachments.is_empty() {
            let uris: Vec<String> = message
                .attachments
                .iter()
                .map(encode_attachment_data_uri)
                .collect();
            obj.insert("attachments".into(), serde_json::json!(uris));
        }

        base
    }

    /// Extract attachment metadata refs from an inbound envelope's data
    /// message. Each ref has the opaque `id` needed for `getAttachment`.
    fn extract_attachment_refs(envelope: &Envelope) -> Vec<AttachmentRef> {
        envelope
            .data_message
            .as_ref()
            .and_then(|d| d.attachments.as_ref())
            .map(|atts| {
                atts.iter()
                    .filter_map(|a| {
                        a.id.as_ref().map(|id| AttachmentRef {
                            id: id.clone(),
                            filename: a.filename.clone(),
                            content_type: a.content_type.clone(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Download a single attachment by id via `getAttachment` JSON-RPC.
    /// Returns the raw decoded bytes.
    async fn download_attachment(
        &self,
        id: &str,
        recipient: Option<&str>,
        group_id: Option<&str>,
    ) -> anyhow::Result<Vec<u8>> {
        let mut params = serde_json::json!({
            "account": &self.account,
            "id": id,
        });
        if let Some(obj) = params.as_object_mut() {
            if let Some(r) = recipient {
                obj.insert("recipient".into(), serde_json::Value::String(r.to_string()));
            }
            if let Some(g) = group_id {
                obj.insert("groupId".into(), serde_json::Value::String(g.to_string()));
            }
        }
        let result = self
            .rpc_request("getAttachment", params)
            .await?
            .ok_or_else(|| anyhow::anyhow!("getAttachment returned no result"))?;
        let b64 = result
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("getAttachment result not a string"))?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64)?;
        Ok(bytes)
    }

    /// Process an envelope and download any inbound attachments.
    ///
    /// Synchronous filtering happens via [`process_envelope`]. When
    /// `ignore_attachments` is false and the envelope has attachments, each
    /// is fetched via `getAttachment` and appended to `ChannelMessage`.
    /// Failed downloads are logged and skipped — they do not abort the message.
    async fn process_envelope_async(&self, envelope: &Envelope) -> Option<ChannelMessage> {
        let mut msg = self.process_envelope(envelope)?;

        if self.ignore_attachments {
            return Some(msg);
        }

        let refs = Self::extract_attachment_refs(envelope);
        if refs.is_empty() {
            return Some(msg);
        }

        let group_id = envelope
            .data_message
            .as_ref()
            .and_then(|d| d.group_info.as_ref())
            .and_then(|g| g.group_id.as_deref());
        let recipient_for_download = if group_id.is_none() {
            Some(msg.sender.as_str())
        } else {
            None
        };

        let mut injected_markers: Vec<String> = Vec::new();

        for r in refs {
            match self
                .download_attachment(&r.id, recipient_for_download, group_id)
                .await
            {
                Ok(bytes) => {
                    let filename = r.filename.clone().unwrap_or_else(|| r.id.clone());
                    let mime = r.content_type.clone();

                    // Persist to workspace when configured so vision-capable
                    // providers (and the media pipeline) can load the file by
                    // path. When workspace_dir is None we fall back silently —
                    // bytes are still available on msg.attachments.
                    if let Some(_ws) = self.workspace_dir.as_ref() {
                        match self
                            .save_inbound_attachment(&r.id, &filename, mime.as_deref(), &bytes)
                            .await
                        {
                            Ok(path) => {
                                injected_markers.push(signal_inbound_marker(
                                    &path,
                                    &filename,
                                    mime.as_deref(),
                                ));
                            }
                            Err(e) => {
                                tracing::warn!(
                                    attachment_id = %r.id,
                                    error = %e,
                                    "Signal inbound attachment save failed"
                                );
                            }
                        }
                    }

                    msg.attachments.push(MediaAttachment {
                        file_name: filename,
                        data: bytes,
                        mime_type: mime,
                    });
                }
                Err(e) => {
                    tracing::warn!("Signal getAttachment failed for {}: {}", r.id, e);
                }
            }
        }

        if !injected_markers.is_empty() {
            let markers = injected_markers.join(" ");
            msg.content = if msg.content.is_empty() {
                markers
            } else {
                format!("{} {}", msg.content, markers)
            };
        }

        Some(msg)
    }

    /// Process a single SSE envelope, returning a ChannelMessage if valid.
    fn process_envelope(&self, envelope: &Envelope) -> Option<ChannelMessage> {
        // Skip story messages when configured
        if self.ignore_stories && envelope.story_message.is_some() {
            return None;
        }

        let data_msg = envelope.data_message.as_ref()?;
        let has_attachments = data_msg.attachments.as_ref().is_some_and(|a| !a.is_empty());
        let has_text = data_msg.message.as_deref().is_some_and(|t| !t.is_empty());

        // Skip attachment-only messages when configured
        if self.ignore_attachments && has_attachments && !has_text {
            return None;
        }

        // Drop envelopes that carry neither text nor attachments (no payload
        // to deliver). Attachment-only messages still proceed — the async
        // download step will populate both bytes and a `[IMAGE:…]` marker.
        if !has_text && !has_attachments {
            return None;
        }

        let text = data_msg.message.as_deref().unwrap_or("");
        let sender = Self::sender(envelope)?;

        if !self.is_sender_allowed(&sender) {
            return None;
        }

        if !self.matches_group(data_msg) {
            return None;
        }

        let target = self.reply_target(data_msg, &sender);

        let timestamp = data_msg
            .timestamp
            .or(envelope.timestamp)
            .unwrap_or_else(|| {
                u64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis(),
                )
                .unwrap_or(u64::MAX)
            });

        Some(ChannelMessage {
            id: format!("sig_{timestamp}"),
            sender: sender.clone(),
            reply_target: target,
            content: text.to_string(),
            channel: "signal".to_string(),
            timestamp: timestamp / 1000, // millis → secs
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        })
    }
}

#[async_trait]
impl Channel for SignalChannel {
    fn name(&self) -> &str {
        "signal"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        // Extract `[KIND:target]` markers from the body before markdown parsing
        // so the marker syntax never reaches the user as visible text.
        let (cleaned_content, marker_targets) = parse_media_markers(&message.content);
        let mut prepared = message.clone();
        prepared.content = cleaned_content;

        let mut params = self.build_send_params(&prepared);

        if !marker_targets.is_empty() {
            let mut resolved: Vec<String> = Vec::with_capacity(marker_targets.len());
            for target in &marker_targets {
                match self.resolve_outbound_marker(target).await {
                    Ok(arg) => resolved.push(arg),
                    Err(e) => {
                        tracing::warn!(
                            target = %target,
                            error = %e,
                            "Signal marker resolution failed; attachment skipped"
                        );
                    }
                }
            }

            if !resolved.is_empty()
                && let Some(obj) = params.as_object_mut()
            {
                let existing = obj.remove("attachments").and_then(|v| match v {
                    serde_json::Value::Array(a) => Some(a),
                    _ => None,
                });
                let mut combined: Vec<serde_json::Value> = existing.unwrap_or_default();
                combined.extend(resolved.into_iter().map(serde_json::Value::String));
                obj.insert("attachments".into(), serde_json::Value::Array(combined));
            }
        }

        self.rpc_request("send", params).await?;
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let mut url = reqwest::Url::parse(&format!("{}/api/v1/events", self.http_url))?;
        url.query_pairs_mut().append_pair("account", &self.account);

        tracing::info!("Signal channel listening via SSE on {}...", self.http_url);

        let mut retry_delay_secs = 2u64;
        let max_delay_secs = 60u64;

        loop {
            let resp = self
                .http_client()
                .get(url.clone())
                .header("Accept", "text/event-stream")
                .send()
                .await;

            let resp = match resp {
                Ok(r) if r.status().is_success() => r,
                Ok(r) => {
                    let status = r.status();
                    let body = r.text().await.unwrap_or_default();
                    tracing::warn!("Signal SSE returned {status}: {body}");
                    tokio::time::sleep(tokio::time::Duration::from_secs(retry_delay_secs)).await;
                    retry_delay_secs = (retry_delay_secs * 2).min(max_delay_secs);
                    continue;
                }
                Err(e) => {
                    tracing::warn!("Signal SSE connect error: {e}, retrying...");
                    tokio::time::sleep(tokio::time::Duration::from_secs(retry_delay_secs)).await;
                    retry_delay_secs = (retry_delay_secs * 2).min(max_delay_secs);
                    continue;
                }
            };

            retry_delay_secs = 2;

            let mut bytes_stream = resp.bytes_stream();
            let mut buffer = String::new();
            let mut current_data = String::new();

            while let Some(chunk) = bytes_stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::debug!("Signal SSE chunk error, reconnecting: {e}");
                        break;
                    }
                };

                let text = match String::from_utf8(chunk.to_vec()) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::debug!("Signal SSE invalid UTF-8, skipping chunk: {}", e);
                        continue;
                    }
                };

                buffer.push_str(&text);

                while let Some(newline_pos) = buffer.find('\n') {
                    let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                    buffer = buffer[newline_pos + 1..].to_string();

                    // Skip SSE comments (keepalive)
                    if line.starts_with(':') {
                        continue;
                    }

                    if line.is_empty() {
                        // Empty line = event boundary, dispatch accumulated data
                        if !current_data.is_empty() {
                            match serde_json::from_str::<SseEnvelope>(&current_data) {
                                Ok(sse) => {
                                    if let Some(ref envelope) = sse.envelope
                                        && let Some(msg) =
                                            self.process_envelope_async(envelope).await
                                        && tx.send(msg).await.is_err()
                                    {
                                        return Ok(());
                                    }
                                }
                                Err(e) => {
                                    tracing::debug!("Signal SSE parse skip: {e}");
                                }
                            }
                            current_data.clear();
                        }
                    } else if let Some(data) = line.strip_prefix("data:") {
                        if !current_data.is_empty() {
                            current_data.push('\n');
                        }
                        current_data.push_str(data.trim_start());
                    }
                    // Ignore "event:", "id:", "retry:" lines
                }
            }

            if !current_data.is_empty() {
                match serde_json::from_str::<SseEnvelope>(&current_data) {
                    Ok(sse) => {
                        if let Some(ref envelope) = sse.envelope
                            && let Some(msg) = self.process_envelope_async(envelope).await
                        {
                            let _ = tx.send(msg).await;
                        }
                    }
                    Err(e) => {
                        tracing::debug!("Signal SSE trailing parse skip: {e}");
                    }
                }
            }

            tracing::debug!("Signal SSE stream ended, reconnecting...");
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        }
    }

    async fn health_check(&self) -> bool {
        let url = format!("{}/api/v1/check", self.http_url);
        let Ok(resp) = self
            .http_client()
            .get(&url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        else {
            return false;
        };
        resp.status().is_success()
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        let params = match Self::parse_recipient_target(recipient) {
            RecipientTarget::Direct(number) => serde_json::json!({
                "recipient": [number],
                "account": &self.account,
            }),
            RecipientTarget::Group(group_id) => serde_json::json!({
                "groupId": group_id,
                "account": &self.account,
            }),
        };
        self.rpc_request("sendTyping", params).await?;
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // signal-cli doesn't have a stop-typing RPC; typing indicators
        // auto-expire after ~15s on the client side.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> SignalChannel {
        SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            None,
            vec!["+1111111111".to_string()],
            false,
            false,
        )
    }

    fn make_channel_with_group(group_id: &str) -> SignalChannel {
        SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Some(group_id.to_string()),
            vec!["*".to_string()],
            true,
            true,
        )
    }

    fn make_envelope(source_number: Option<&str>, message: Option<&str>) -> Envelope {
        Envelope {
            source: source_number.map(String::from),
            source_number: source_number.map(String::from),
            data_message: message.map(|m| DataMessage {
                message: Some(m.to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        }
    }

    #[test]
    fn creates_with_correct_fields() {
        let ch = make_channel();
        assert_eq!(ch.http_url, "http://127.0.0.1:8686");
        assert_eq!(ch.account, "+1234567890");
        assert!(ch.group_id.is_none());
        assert_eq!(ch.allowed_from.len(), 1);
        assert!(!ch.ignore_attachments);
        assert!(!ch.ignore_stories);
    }

    #[test]
    fn strips_trailing_slash() {
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686/".to_string(),
            "+1234567890".to_string(),
            None,
            vec![],
            false,
            false,
        );
        assert_eq!(ch.http_url, "http://127.0.0.1:8686");
    }

    #[test]
    fn wildcard_allows_anyone() {
        let ch = make_channel_with_group("dm");
        assert!(ch.is_sender_allowed("+9999999999"));
    }

    #[test]
    fn specific_sender_allowed() {
        let ch = make_channel();
        assert!(ch.is_sender_allowed("+1111111111"));
    }

    #[test]
    fn unknown_sender_denied() {
        let ch = make_channel();
        assert!(!ch.is_sender_allowed("+9999999999"));
    }

    #[test]
    fn empty_allowlist_denies_all() {
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            None,
            vec![],
            false,
            false,
        );
        assert!(!ch.is_sender_allowed("+1111111111"));
    }

    #[test]
    fn name_returns_signal() {
        let ch = make_channel();
        assert_eq!(ch.name(), "signal");
    }

    #[test]
    fn matches_group_no_group_id_accepts_all() {
        let ch = make_channel();
        let dm = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: None,
            attachments: None,
        };
        assert!(ch.matches_group(&dm));

        let group = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: Some(GroupInfo {
                group_id: Some("group123".to_string()),
            }),
            attachments: None,
        };
        assert!(ch.matches_group(&group));
    }

    #[test]
    fn matches_group_filters_group() {
        let ch = make_channel_with_group("group123");
        let matching = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: Some(GroupInfo {
                group_id: Some("group123".to_string()),
            }),
            attachments: None,
        };
        assert!(ch.matches_group(&matching));

        let non_matching = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: Some(GroupInfo {
                group_id: Some("other_group".to_string()),
            }),
            attachments: None,
        };
        assert!(!ch.matches_group(&non_matching));
    }

    #[test]
    fn matches_group_dm_keyword() {
        let ch = make_channel_with_group("dm");
        let dm = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: None,
            attachments: None,
        };
        assert!(ch.matches_group(&dm));

        let group = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: Some(GroupInfo {
                group_id: Some("group123".to_string()),
            }),
            attachments: None,
        };
        assert!(!ch.matches_group(&group));
    }

    #[test]
    fn reply_target_dm() {
        let ch = make_channel();
        let dm = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: None,
            attachments: None,
        };
        assert_eq!(ch.reply_target(&dm, "+1111111111"), "+1111111111");
    }

    #[test]
    fn reply_target_group() {
        let ch = make_channel();
        let group = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: Some(GroupInfo {
                group_id: Some("group123".to_string()),
            }),
            attachments: None,
        };
        assert_eq!(ch.reply_target(&group, "+1111111111"), "group:group123");
    }

    #[test]
    fn parse_recipient_target_e164_is_direct() {
        assert_eq!(
            SignalChannel::parse_recipient_target("+1234567890"),
            RecipientTarget::Direct("+1234567890".to_string())
        );
    }

    #[test]
    fn parse_recipient_target_prefixed_group_is_group() {
        assert_eq!(
            SignalChannel::parse_recipient_target("group:abc123"),
            RecipientTarget::Group("abc123".to_string())
        );
    }

    #[test]
    fn parse_recipient_target_uuid_is_direct() {
        let uuid = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
        assert_eq!(
            SignalChannel::parse_recipient_target(uuid),
            RecipientTarget::Direct(uuid.to_string())
        );
    }

    #[test]
    fn parse_recipient_target_non_e164_plus_is_group() {
        assert_eq!(
            SignalChannel::parse_recipient_target("+abc123"),
            RecipientTarget::Group("+abc123".to_string())
        );
    }

    #[test]
    fn is_uuid_valid() {
        assert!(SignalChannel::is_uuid(
            "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
        ));
        assert!(SignalChannel::is_uuid(
            "00000000-0000-0000-0000-000000000000"
        ));
    }

    #[test]
    fn is_uuid_invalid() {
        assert!(!SignalChannel::is_uuid("+1234567890"));
        assert!(!SignalChannel::is_uuid("not-a-uuid"));
        assert!(!SignalChannel::is_uuid("group:abc123"));
        assert!(!SignalChannel::is_uuid(""));
    }

    #[test]
    fn sender_prefers_source_number() {
        let env = Envelope {
            source: Some("uuid-123".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: None,
            story_message: None,
            timestamp: Some(1000),
        };
        assert_eq!(SignalChannel::sender(&env), Some("+1111111111".to_string()));
    }

    #[test]
    fn sender_falls_back_to_source() {
        let env = Envelope {
            source: Some("uuid-123".to_string()),
            source_number: None,
            data_message: None,
            story_message: None,
            timestamp: Some(1000),
        };
        assert_eq!(SignalChannel::sender(&env), Some("uuid-123".to_string()));
    }

    #[test]
    fn process_envelope_uuid_sender_dm() {
        let uuid = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            None,
            vec!["*".to_string()],
            false,
            false,
        );
        let env = Envelope {
            source: Some(uuid.to_string()),
            source_number: None,
            data_message: Some(DataMessage {
                message: Some("Hello from privacy user".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let msg = ch.process_envelope(&env).unwrap();
        assert_eq!(msg.sender, uuid);
        assert_eq!(msg.reply_target, uuid);
        assert_eq!(msg.content, "Hello from privacy user");

        // Verify reply routing: UUID sender in DM should route as Direct
        let target = SignalChannel::parse_recipient_target(&msg.reply_target);
        assert_eq!(target, RecipientTarget::Direct(uuid.to_string()));
    }

    #[test]
    fn process_envelope_uuid_sender_in_group() {
        let uuid = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Some("testgroup".to_string()),
            vec!["*".to_string()],
            false,
            false,
        );
        let env = Envelope {
            source: Some(uuid.to_string()),
            source_number: None,
            data_message: Some(DataMessage {
                message: Some("Group msg from privacy user".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: Some(GroupInfo {
                    group_id: Some("testgroup".to_string()),
                }),
                attachments: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let msg = ch.process_envelope(&env).unwrap();
        assert_eq!(msg.sender, uuid);
        assert_eq!(msg.reply_target, "group:testgroup");

        // Verify reply routing: group message should still route as Group
        let target = SignalChannel::parse_recipient_target(&msg.reply_target);
        assert_eq!(target, RecipientTarget::Group("testgroup".to_string()));
    }

    #[test]
    fn sender_none_when_both_missing() {
        let env = Envelope {
            source: None,
            source_number: None,
            data_message: None,
            story_message: None,
            timestamp: None,
        };
        assert_eq!(SignalChannel::sender(&env), None);
    }

    #[test]
    fn process_envelope_valid_dm() {
        let ch = make_channel();
        let env = make_envelope(Some("+1111111111"), Some("Hello!"));
        let msg = ch.process_envelope(&env).unwrap();
        assert_eq!(msg.content, "Hello!");
        assert_eq!(msg.sender, "+1111111111");
        assert_eq!(msg.channel, "signal");
    }

    #[test]
    fn process_envelope_denied_sender() {
        let ch = make_channel();
        let env = make_envelope(Some("+9999999999"), Some("Hello!"));
        assert!(ch.process_envelope(&env).is_none());
    }

    #[test]
    fn process_envelope_empty_message() {
        let ch = make_channel();
        let env = make_envelope(Some("+1111111111"), Some(""));
        assert!(ch.process_envelope(&env).is_none());
    }

    #[test]
    fn process_envelope_no_data_message() {
        let ch = make_channel();
        let env = make_envelope(Some("+1111111111"), None);
        assert!(ch.process_envelope(&env).is_none());
    }

    #[test]
    fn process_envelope_skips_stories() {
        let ch = make_channel_with_group("dm");
        let mut env = make_envelope(Some("+1111111111"), Some("story text"));
        env.story_message = Some(serde_json::json!({}));
        assert!(ch.process_envelope(&env).is_none());
    }

    #[test]
    fn process_envelope_skips_attachment_only() {
        let ch = make_channel_with_group("dm");
        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: None,
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: Some(vec![SignalAttachment {
                    content_type: Some("image/png".to_string()),
                    filename: None,
                    id: Some("att1".to_string()),
                }]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        assert!(ch.process_envelope(&env).is_none());
    }

    #[test]
    fn sse_envelope_deserializes() {
        let json = r#"{
            "envelope": {
                "source": "+1111111111",
                "sourceNumber": "+1111111111",
                "timestamp": 1700000000000,
                "dataMessage": {
                    "message": "Hello Signal!",
                    "timestamp": 1700000000000
                }
            }
        }"#;
        let sse: SseEnvelope = serde_json::from_str(json).unwrap();
        let env = sse.envelope.unwrap();
        assert_eq!(env.source_number.as_deref(), Some("+1111111111"));
        let dm = env.data_message.unwrap();
        assert_eq!(dm.message.as_deref(), Some("Hello Signal!"));
    }

    #[test]
    fn sse_envelope_deserializes_group() {
        let json = r#"{
            "envelope": {
                "sourceNumber": "+2222222222",
                "dataMessage": {
                    "message": "Group msg",
                    "groupInfo": {
                        "groupId": "abc123"
                    }
                }
            }
        }"#;
        let sse: SseEnvelope = serde_json::from_str(json).unwrap();
        let env = sse.envelope.unwrap();
        let dm = env.data_message.unwrap();
        assert_eq!(
            dm.group_info.as_ref().unwrap().group_id.as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn envelope_defaults() {
        let json = r#"{}"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        assert!(env.source.is_none());
        assert!(env.source_number.is_none());
        assert!(env.data_message.is_none());
        assert!(env.story_message.is_none());
        assert!(env.timestamp.is_none());
    }

    // ── markdown → textStyles ────────────────────────────────────

    #[test]
    fn markdown_plain_text_unchanged() {
        let (text, styles) = markdown_to_signal_text("hello world");
        assert_eq!(text, "hello world");
        assert!(styles.is_empty());
    }

    #[test]
    fn markdown_bold() {
        let (text, styles) = markdown_to_signal_text("**hi**");
        assert_eq!(text, "hi");
        assert_eq!(styles, vec!["0:2:BOLD".to_string()]);
    }

    #[test]
    fn markdown_italic_asterisk() {
        let (text, styles) = markdown_to_signal_text("*hi*");
        assert_eq!(text, "hi");
        assert_eq!(styles, vec!["0:2:ITALIC".to_string()]);
    }

    #[test]
    fn markdown_italic_underscore() {
        let (text, styles) = markdown_to_signal_text("_hi_");
        assert_eq!(text, "hi");
        assert_eq!(styles, vec!["0:2:ITALIC".to_string()]);
    }

    #[test]
    fn markdown_strikethrough() {
        let (text, styles) = markdown_to_signal_text("~~hi~~");
        assert_eq!(text, "hi");
        assert_eq!(styles, vec!["0:2:STRIKETHROUGH".to_string()]);
    }

    #[test]
    fn markdown_spoiler() {
        let (text, styles) = markdown_to_signal_text("||hi||");
        assert_eq!(text, "hi");
        assert_eq!(styles, vec!["0:2:SPOILER".to_string()]);
    }

    #[test]
    fn markdown_monospace() {
        let (text, styles) = markdown_to_signal_text("`hi`");
        assert_eq!(text, "hi");
        assert_eq!(styles, vec!["0:2:MONOSPACE".to_string()]);
    }

    #[test]
    fn markdown_offset_in_middle() {
        let (text, styles) = markdown_to_signal_text("Hello **world**!");
        assert_eq!(text, "Hello world!");
        assert_eq!(styles, vec!["6:5:BOLD".to_string()]);
    }

    #[test]
    fn markdown_nested_bold_italic() {
        let (text, styles) = markdown_to_signal_text("**a _b_ c**");
        assert_eq!(text, "a b c");
        // BOLD covers "a b c" (5 chars), ITALIC covers "b" at offset 2 (1 char).
        // Sorted lexicographically: "0:5:BOLD" < "2:1:ITALIC".
        assert_eq!(
            styles,
            vec!["0:5:BOLD".to_string(), "2:1:ITALIC".to_string()]
        );
    }

    #[test]
    fn markdown_multiple_spans() {
        let (text, styles) = markdown_to_signal_text("**bold** then *italic*");
        assert_eq!(text, "bold then italic");
        assert_eq!(
            styles,
            vec!["0:4:BOLD".to_string(), "10:6:ITALIC".to_string()]
        );
    }

    #[test]
    fn markdown_utf16_offsets_with_accent() {
        // 'é' is 1 UTF-16 code unit (BMP), so offsets stay 0..4.
        let (text, styles) = markdown_to_signal_text("**café**");
        assert_eq!(text, "café");
        assert_eq!(styles, vec!["0:4:BOLD".to_string()]);
    }

    #[test]
    fn markdown_utf16_offsets_with_emoji_surrogate_pair() {
        // '👋' (U+1F44B) is 2 UTF-16 code units (surrogate pair).
        let (text, styles) = markdown_to_signal_text("**👋**");
        assert_eq!(text, "👋");
        assert_eq!(styles, vec!["0:2:BOLD".to_string()]);
    }

    #[test]
    fn markdown_unmatched_opener_kept_literal() {
        // Single `**` with no closer — bytes remain in output, no styles.
        let (text, styles) = markdown_to_signal_text("**hi");
        assert_eq!(text, "**hi");
        assert!(styles.is_empty());
    }

    #[test]
    fn markdown_arithmetic_star_not_treated_as_italic() {
        // "5*5" — `*` is preceded by alphanumeric, fails left-flanking for opener.
        let (text, styles) = markdown_to_signal_text("5*5=25");
        assert_eq!(text, "5*5=25");
        assert!(styles.is_empty());
    }

    #[test]
    fn markdown_backslash_escape_star() {
        let (text, styles) = markdown_to_signal_text("\\*not italic\\*");
        assert_eq!(text, "*not italic*");
        assert!(styles.is_empty());
    }

    #[test]
    fn markdown_backslash_escape_backslash() {
        let (text, styles) = markdown_to_signal_text("a\\\\b");
        assert_eq!(text, "a\\b");
        assert!(styles.is_empty());
    }

    #[test]
    fn markdown_inside_monospace_is_literal() {
        // Markdown chars between backticks are taken verbatim.
        let (text, styles) = markdown_to_signal_text("`**not bold**`");
        assert_eq!(text, "**not bold**");
        assert_eq!(styles, vec!["0:12:MONOSPACE".to_string()]);
    }

    // ── attachment data URI encoding ─────────────────────────────

    #[test]
    fn data_uri_for_png() {
        let att = MediaAttachment {
            file_name: "cat.png".to_string(),
            data: vec![0x89, 0x50, 0x4e, 0x47],
            mime_type: Some("image/png".to_string()),
        };
        let uri = encode_attachment_data_uri(&att);
        // Base64 of [0x89,0x50,0x4e,0x47] is "iVBORw==".
        assert_eq!(uri, "data:image/png;filename=cat.png;base64,iVBORw==");
    }

    #[test]
    fn data_uri_falls_back_to_octet_stream() {
        let att = MediaAttachment {
            file_name: "blob.bin".to_string(),
            data: vec![0x01, 0x02, 0x03],
            mime_type: None,
        };
        let uri = encode_attachment_data_uri(&att);
        assert!(uri.starts_with("data:application/octet-stream;filename=blob.bin;base64,"));
    }

    // ── build_send_params ────────────────────────────────────────

    #[test]
    fn send_params_direct_plain_text() {
        let ch = make_channel();
        let msg = SendMessage::new("hello", "+1111111111");
        let params = ch.build_send_params(&msg);
        assert_eq!(params["recipient"], serde_json::json!(["+1111111111"]));
        assert_eq!(params["account"], serde_json::json!("+1234567890"));
        assert_eq!(params["message"], serde_json::json!("hello"));
        assert!(params.get("textStyles").is_none());
        assert!(params.get("attachments").is_none());
        assert!(params.get("groupId").is_none());
    }

    #[test]
    fn send_params_group() {
        let ch = make_channel();
        let msg = SendMessage::new("hi", "group:abc123");
        let params = ch.build_send_params(&msg);
        assert_eq!(params["groupId"], serde_json::json!("abc123"));
        assert!(params.get("recipient").is_none());
    }

    #[test]
    fn send_params_with_image_attachment() {
        let ch = make_channel();
        let msg = SendMessage::new("look", "+1111111111").with_attachments(vec![MediaAttachment {
            file_name: "cat.png".to_string(),
            data: vec![0x89, 0x50, 0x4e, 0x47],
            mime_type: Some("image/png".to_string()),
        }]);
        let params = ch.build_send_params(&msg);
        let atts = params["attachments"].as_array().expect("attachments array");
        assert_eq!(atts.len(), 1);
        assert_eq!(
            atts[0],
            serde_json::json!("data:image/png;filename=cat.png;base64,iVBORw==")
        );
        assert_eq!(params["message"], serde_json::json!("look"));
    }

    #[test]
    fn send_params_with_multiple_attachments() {
        let ch = make_channel();
        let msg = SendMessage::new("media", "+1111111111").with_attachments(vec![
            MediaAttachment {
                file_name: "a.gif".to_string(),
                data: vec![0x47, 0x49, 0x46],
                mime_type: Some("image/gif".to_string()),
            },
            MediaAttachment {
                file_name: "b.mp4".to_string(),
                data: vec![0x00, 0x01],
                mime_type: Some("video/mp4".to_string()),
            },
        ]);
        let params = ch.build_send_params(&msg);
        let atts = params["attachments"].as_array().unwrap();
        assert_eq!(atts.len(), 2);
        assert!(atts[0].as_str().unwrap().starts_with("data:image/gif"));
        assert!(atts[1].as_str().unwrap().starts_with("data:video/mp4"));
    }

    #[test]
    fn send_params_attachment_only_no_message_field() {
        // Empty content + attachment: send attachment without `message` key.
        let ch = make_channel();
        let msg = SendMessage::new("", "+1111111111").with_attachments(vec![MediaAttachment {
            file_name: "x.jpg".to_string(),
            data: vec![0xff],
            mime_type: Some("image/jpeg".to_string()),
        }]);
        let params = ch.build_send_params(&msg);
        assert!(params.get("message").is_none());
        assert!(params["attachments"].as_array().unwrap().len() == 1);
    }

    #[test]
    fn send_params_with_markdown_formatting() {
        let ch = make_channel();
        let msg = SendMessage::new("**hi** _there_", "+1111111111");
        let params = ch.build_send_params(&msg);
        assert_eq!(params["message"], serde_json::json!("hi there"));
        let styles = params["textStyles"].as_array().unwrap();
        assert_eq!(styles.len(), 2);
        assert!(styles.contains(&serde_json::json!("0:2:BOLD")));
        assert!(styles.contains(&serde_json::json!("3:5:ITALIC")));
    }

    #[test]
    fn send_params_combines_markdown_and_attachment() {
        let ch = make_channel();
        let msg = SendMessage::new("**bold** caption", "+1111111111").with_attachments(vec![
            MediaAttachment {
                file_name: "p.png".to_string(),
                data: vec![0x01],
                mime_type: Some("image/png".to_string()),
            },
        ]);
        let params = ch.build_send_params(&msg);
        assert_eq!(params["message"], serde_json::json!("bold caption"));
        assert_eq!(params["textStyles"], serde_json::json!(["0:4:BOLD"]));
        assert_eq!(params["attachments"].as_array().unwrap().len(), 1);
    }

    // ── inbound attachment parsing ───────────────────────────────

    #[test]
    fn signal_attachment_deserializes() {
        let json = r#"{
            "contentType": "image/jpeg",
            "filename": "IMG_0001.jpg",
            "id": "abc123",
            "size": 48213,
            "width": 1024,
            "height": 768
        }"#;
        let att: SignalAttachment = serde_json::from_str(json).unwrap();
        assert_eq!(att.content_type.as_deref(), Some("image/jpeg"));
        assert_eq!(att.filename.as_deref(), Some("IMG_0001.jpg"));
        assert_eq!(att.id.as_deref(), Some("abc123"));
    }

    #[test]
    fn extract_attachment_refs_returns_ids() {
        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: Some("look".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: Some(vec![
                    SignalAttachment {
                        content_type: Some("image/jpeg".to_string()),
                        filename: Some("photo.jpg".to_string()),
                        id: Some("att-id-1".to_string()),
                    },
                    SignalAttachment {
                        content_type: Some("image/gif".to_string()),
                        filename: None,
                        id: Some("att-id-2".to_string()),
                    },
                ]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let refs = SignalChannel::extract_attachment_refs(&env);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].id, "att-id-1");
        assert_eq!(refs[0].filename.as_deref(), Some("photo.jpg"));
        assert_eq!(refs[0].content_type.as_deref(), Some("image/jpeg"));
        assert_eq!(refs[1].id, "att-id-2");
        assert!(refs[1].filename.is_none());
    }

    #[test]
    fn extract_attachment_refs_skips_entries_without_id() {
        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: Some("hi".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: Some(vec![SignalAttachment {
                    content_type: Some("image/png".to_string()),
                    filename: None,
                    id: None,
                }]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        assert!(SignalChannel::extract_attachment_refs(&env).is_empty());
    }

    #[test]
    fn process_envelope_with_text_and_attachment_keeps_text() {
        // Sync filter keeps the text-bearing message; attachment refs are
        // populated separately. ChannelMessage.attachments is empty until the
        // async download step (covered by the wiremock test below).
        let ch = make_channel();
        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: Some("look".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: Some(vec![SignalAttachment {
                    content_type: Some("image/png".to_string()),
                    filename: Some("cat.png".to_string()),
                    id: Some("att-1".to_string()),
                }]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let msg = ch.process_envelope(&env).expect("text+attachment passes");
        assert_eq!(msg.content, "look");
        assert!(msg.attachments.is_empty());
    }

    // ── download + async envelope processing (wiremock) ──────────

    #[tokio::test]
    async fn process_envelope_async_downloads_attachment() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        // Decoded bytes "hello".
        let b64 = "aGVsbG8=";
        Mock::given(method("POST"))
            .and(path("/api/v1/rpc"))
            .and(header("content-type", "application/json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "result": b64,
                "id": "req-1",
            })))
            .mount(&server)
            .await;

        let ch = SignalChannel::new(
            server.uri(),
            "+1234567890".to_string(),
            None,
            vec!["*".to_string()],
            false,
            false,
        );

        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: Some("look".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: Some(vec![SignalAttachment {
                    content_type: Some("image/png".to_string()),
                    filename: Some("cat.png".to_string()),
                    id: Some("att-1".to_string()),
                }]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };

        let msg = ch
            .process_envelope_async(&env)
            .await
            .expect("envelope yields message");
        assert_eq!(msg.content, "look");
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].file_name, "cat.png");
        assert_eq!(msg.attachments[0].data, b"hello");
        assert_eq!(msg.attachments[0].mime_type.as_deref(), Some("image/png"));
    }

    #[tokio::test]
    async fn process_envelope_async_skips_download_when_ignore_attachments() {
        use wiremock::MockServer;
        // ignore_attachments=true: text+media envelope still yields a message,
        // but attachments are not downloaded (the mock endpoint must not be
        // hit; wiremock fails the test if an unmatched call is made).
        let server = MockServer::start().await;
        // No mocks mounted — any RPC call would surface as a 404 from wiremock.

        let ch = SignalChannel::new(
            server.uri(),
            "+1234567890".to_string(),
            None,
            vec!["*".to_string()],
            true,  // ignore_attachments
            false, // ignore_stories
        );

        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: Some("hi".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: Some(vec![SignalAttachment {
                    content_type: Some("image/png".to_string()),
                    filename: Some("cat.png".to_string()),
                    id: Some("att-1".to_string()),
                }]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };

        let msg = ch.process_envelope_async(&env).await.unwrap();
        assert_eq!(msg.content, "hi");
        assert!(msg.attachments.is_empty());
    }

    #[tokio::test]
    async fn process_envelope_async_continues_when_one_download_fails() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        // First attachment fails (RPC error); second succeeds.
        Mock::given(method("POST"))
            .and(path("/api/v1/rpc"))
            .and(body_partial_json(
                serde_json::json!({"params": {"id": "bad"}}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "error": {"code": -1, "message": "no such attachment"},
                "id": "req-bad",
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/rpc"))
            .and(body_partial_json(
                serde_json::json!({"params": {"id": "good"}}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "result": "Z29vZA==", // base64("good")
                "id": "req-good",
            })))
            .mount(&server)
            .await;

        let ch = SignalChannel::new(
            server.uri(),
            "+1234567890".to_string(),
            None,
            vec!["*".to_string()],
            false,
            false,
        );

        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: Some("two media".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: Some(vec![
                    SignalAttachment {
                        content_type: Some("image/png".to_string()),
                        filename: Some("a.png".to_string()),
                        id: Some("bad".to_string()),
                    },
                    SignalAttachment {
                        content_type: Some("image/gif".to_string()),
                        filename: Some("b.gif".to_string()),
                        id: Some("good".to_string()),
                    },
                ]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };

        let msg = ch.process_envelope_async(&env).await.unwrap();
        // Failed downloads are logged & skipped — the message is still delivered.
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].file_name, "b.gif");
        assert_eq!(msg.attachments[0].data, b"good");
    }

    #[tokio::test]
    async fn send_with_markdown_and_image_posts_correct_rpc() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/v1/rpc"))
            .and(body_partial_json(serde_json::json!({
                "method": "send",
                "params": {
                    "account": "+1234567890",
                    "recipient": ["+1111111111"],
                    "message": "Look at this big news!",
                    "textStyles": ["13:3:BOLD"],
                    "attachments": ["data:image/png;filename=cat.png;base64,AQ=="]
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "result": {"timestamp": 1_700_000_000_000_u64},
                "id": "req-1",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let ch = SignalChannel::new(
            server.uri(),
            "+1234567890".to_string(),
            None,
            vec!["*".to_string()],
            false,
            false,
        );

        let msg =
            SendMessage::new("Look at this **big** news!", "+1111111111").with_attachments(vec![
                MediaAttachment {
                    file_name: "cat.png".to_string(),
                    data: vec![0x01],
                    mime_type: Some("image/png".to_string()),
                },
            ]);

        ch.send(&msg).await.expect("send succeeds");
        // wiremock verifies on drop that .expect(1) was satisfied.
    }

    // ── outbound media-marker parsing ────────────────────────────

    #[test]
    fn parse_markers_extracts_image_path() {
        let (text, targets) = parse_media_markers("look [IMAGE:/tmp/cat.png]");
        assert_eq!(text, "look");
        assert_eq!(targets, vec!["/tmp/cat.png".to_string()]);
    }

    #[test]
    fn parse_markers_accepts_photo_video_audio_voice_document_file() {
        let (text, targets) = parse_media_markers(
            "a [PHOTO:/a.jpg] b [VIDEO:/b.mp4] c [AUDIO:/c.mp3] d [VOICE:/d.ogg] e [DOCUMENT:/e.pdf] f [FILE:/f.zip]",
        );
        assert_eq!(text, "a b c d e f");
        assert_eq!(
            targets,
            vec![
                "/a.jpg".to_string(),
                "/b.mp4".to_string(),
                "/c.mp3".to_string(),
                "/d.ogg".to_string(),
                "/e.pdf".to_string(),
                "/f.zip".to_string(),
            ]
        );
    }

    #[test]
    fn parse_markers_preserves_unknown_brackets() {
        let (text, targets) = parse_media_markers("see [note: hello] and [IMAGE:/cat.png]");
        // Non-media bracket kept verbatim; media marker stripped and harvested.
        assert_eq!(text, "see [note: hello] and");
        assert_eq!(targets, vec!["/cat.png".to_string()]);
    }

    #[test]
    fn parse_markers_ignores_empty_target() {
        let (text, targets) = parse_media_markers("empty [IMAGE:] marker");
        // Empty-target marker passes through as literal text.
        assert_eq!(text, "empty [IMAGE:] marker");
        assert!(targets.is_empty());
    }

    #[test]
    fn parse_markers_plain_text_unchanged() {
        let (text, targets) = parse_media_markers("plain [hi] text");
        assert_eq!(text, "plain [hi] text");
        assert!(targets.is_empty());
    }

    #[test]
    fn parse_markers_collapses_gaps_left_by_removed_marker() {
        let (text, targets) = parse_media_markers("here   [IMAGE:/p.png]   there");
        assert_eq!(text, "here there");
        assert_eq!(targets, vec!["/p.png".to_string()]);
    }

    // ── outbound attachment resolution ───────────────────────────

    #[tokio::test]
    async fn resolve_outbound_marker_remaps_workspace_prefix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file_path = tmp.path().join("cat.png");
        tokio::fs::write(&file_path, b"\x89PNG").await.unwrap();

        let ch = make_channel().with_workspace_dir(tmp.path().to_path_buf());
        let resolved = ch
            .resolve_outbound_marker("/workspace/cat.png")
            .await
            .expect("remap succeeds");
        assert_eq!(resolved, file_path.to_string_lossy());
    }

    #[tokio::test]
    async fn resolve_outbound_marker_passes_absolute_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file_path = tmp.path().join("pic.jpg");
        tokio::fs::write(&file_path, b"\xff\xd8").await.unwrap();

        let ch = make_channel();
        let resolved = ch
            .resolve_outbound_marker(file_path.to_str().unwrap())
            .await
            .expect("path accepted");
        assert_eq!(resolved, file_path.to_string_lossy());
    }

    #[tokio::test]
    async fn resolve_outbound_marker_rejects_missing_path() {
        let ch = make_channel();
        let err = ch
            .resolve_outbound_marker("/definitely/missing/file.png")
            .await
            .expect_err("missing file errors");
        assert!(format!("{err}").contains("not found"));
    }

    #[tokio::test]
    async fn resolve_outbound_marker_downloads_http_url_to_data_uri() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/cat.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(vec![0x89, 0x50, 0x4e, 0x47])
                    .insert_header("content-type", "image/png"),
            )
            .mount(&server)
            .await;

        let ch = make_channel();
        let url = format!("{}/cat.png", server.uri());
        let resolved = ch.resolve_outbound_marker(&url).await.expect("download ok");
        assert_eq!(resolved, "data:image/png;filename=cat.png;base64,iVBORw==");
    }

    #[tokio::test]
    async fn send_expands_image_marker_to_attachment_path() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let tmp = tempfile::TempDir::new().unwrap();
        let img_path = tmp.path().join("photo.png");
        tokio::fs::write(&img_path, b"\x89PNG").await.unwrap();
        let img_path_str = img_path.to_string_lossy().into_owned();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/rpc"))
            .and(body_partial_json(serde_json::json!({
                "method": "send",
                "params": {
                    "account": "+1234567890",
                    "recipient": ["+1111111111"],
                    "message": "look",
                    "attachments": [img_path_str.clone()]
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "result": {"timestamp": 1_700_000_000_000_u64},
                "id": "r1",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let ch = SignalChannel::new(
            server.uri(),
            "+1234567890".to_string(),
            None,
            vec!["*".to_string()],
            false,
            false,
        );

        let msg = SendMessage::new(format!("look [IMAGE:{img_path_str}]"), "+1111111111");
        ch.send(&msg).await.expect("send ok");
    }

    // ── inbound attachment persistence + marker injection ────────

    #[tokio::test]
    async fn inbound_image_persists_and_injects_marker() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let tmp = tempfile::TempDir::new().unwrap();
        let server = MockServer::start().await;

        // base64("PNGDATA") = "UE5HREFUQQ=="
        Mock::given(method("POST"))
            .and(path("/api/v1/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "result": "UE5HREFUQQ==",
                "id": "d1",
            })))
            .mount(&server)
            .await;

        let ch = SignalChannel::new(
            server.uri(),
            "+1234567890".to_string(),
            None,
            vec!["*".to_string()],
            false,
            false,
        )
        .with_workspace_dir(tmp.path().to_path_buf());

        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: Some("look".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: Some(vec![SignalAttachment {
                    content_type: Some("image/png".to_string()),
                    filename: Some("cat.png".to_string()),
                    id: Some("att-1".to_string()),
                }]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };

        let msg = ch.process_envelope_async(&env).await.unwrap();

        // File was written under workspace_dir/signal_inbound/<id>.png.
        let expected_path = tmp.path().join(SIGNAL_INBOUND_SUBDIR).join("att-1.png");
        assert!(expected_path.exists());
        let on_disk = tokio::fs::read(&expected_path).await.unwrap();
        assert_eq!(on_disk, b"PNGDATA");

        // Marker is appended to content so the LLM can see the image path.
        let expected_marker = format!("[IMAGE:{}]", expected_path.display());
        assert!(
            msg.content.contains(&expected_marker),
            "content missing marker: {:?}",
            msg.content
        );
        assert!(msg.content.starts_with("look"));

        // Raw bytes are still exposed to the media pipeline.
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].data, b"PNGDATA");
    }

    #[tokio::test]
    async fn inbound_attachment_only_message_delivers_with_marker() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let tmp = tempfile::TempDir::new().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "result": "Z2lm", // base64("gif")
                "id": "d2",
            })))
            .mount(&server)
            .await;

        let ch = SignalChannel::new(
            server.uri(),
            "+1234567890".to_string(),
            None,
            vec!["*".to_string()],
            false,
            false,
        )
        .with_workspace_dir(tmp.path().to_path_buf());

        // Photo without any caption text — previously dropped by process_envelope.
        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: None,
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: Some(vec![SignalAttachment {
                    content_type: Some("image/gif".to_string()),
                    filename: Some("wave.gif".to_string()),
                    id: Some("att-2".to_string()),
                }]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };

        let msg = ch
            .process_envelope_async(&env)
            .await
            .expect("attachment-only still delivers");
        assert!(msg.content.starts_with("[IMAGE:"));
        assert_eq!(msg.attachments.len(), 1);
    }

    #[tokio::test]
    async fn inbound_non_image_uses_document_marker() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let tmp = tempfile::TempDir::new().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "result": "UERG", // base64("PDF") ≈
                "id": "d3",
            })))
            .mount(&server)
            .await;

        let ch = SignalChannel::new(
            server.uri(),
            "+1234567890".to_string(),
            None,
            vec!["*".to_string()],
            false,
            false,
        )
        .with_workspace_dir(tmp.path().to_path_buf());

        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: Some("here is the doc".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: Some(vec![SignalAttachment {
                    content_type: Some("application/pdf".to_string()),
                    filename: Some("invoice.pdf".to_string()),
                    id: Some("att-3".to_string()),
                }]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };

        let msg = ch.process_envelope_async(&env).await.unwrap();
        let expected_path = tmp.path().join(SIGNAL_INBOUND_SUBDIR).join("att-3.pdf");
        assert!(
            msg.content.contains(&format!(
                "[Document: invoice.pdf] {}",
                expected_path.display()
            )),
            "content missing doc marker: {:?}",
            msg.content
        );
    }

    #[tokio::test]
    async fn inbound_without_workspace_dir_keeps_bytes_only() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "result": "YWJj",
                "id": "d4",
            })))
            .mount(&server)
            .await;

        // No workspace_dir configured — behaviour falls back to pre-feature:
        // bytes present on msg.attachments, no marker injected.
        let ch = SignalChannel::new(
            server.uri(),
            "+1234567890".to_string(),
            None,
            vec!["*".to_string()],
            false,
            false,
        );

        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: Some("hi".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: Some(vec![SignalAttachment {
                    content_type: Some("image/png".to_string()),
                    filename: Some("p.png".to_string()),
                    id: Some("att-4".to_string()),
                }]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };

        let msg = ch.process_envelope_async(&env).await.unwrap();
        assert_eq!(msg.content, "hi"); // no marker appended
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].data, b"abc");
    }
}
