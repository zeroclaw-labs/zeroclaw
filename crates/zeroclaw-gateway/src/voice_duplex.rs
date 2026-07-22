//! Voice duplex pipeline support for `/ws/chat` sessions.
//!
//! `ws.rs` recognizes the voice wire contract when the
//! `gateway-voice-duplex` cargo feature is enabled (it is a default
//! feature): a client `{"type":"speech_end","transcript":...,"images":[..]?}`
//! frame starts a normal agent turn flagged as a *voice turn*; the streamed
//! assistant text is teed into a [`SentenceChunker`], each sentence unit is
//! synthesized through the agent's configured TTS provider, and the audio is
//! emitted as ordered `tts_chunk` frames. `{"type":"barge_in"}` cancels the
//! running turn via the session's cancellation token and emits `tts_cancel`.
//!
//! This module owns the text-side machinery: sentence chunking, markdown
//! stripping for speech, image data-URI validation, and the resolved TTS
//! binding for a voice turn. The socket plumbing lives in `ws.rs`.

use std::sync::Arc;

use zeroclaw_channels::tts::TtsManager;
use zeroclaw_config::schema::Config;

/// Maximum images accepted on a single `speech_end` / `message` frame.
/// Mirrors the `[multimodal]` default cap.
pub const MAX_IMAGES_PER_TURN: usize = 4;

/// Maximum decoded size per image (5 MB), estimated from the base64
/// payload length without decoding. Mirrors the `[multimodal]` default.
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

/// Minimum sentence-unit length (in characters) before a TTS unit is
/// emitted; shorter sentences are merged with the following one so tiny
/// fragments ("Yes.", "OK!") don't produce choppy audio.
pub const DEFAULT_MIN_TTS_UNIT_CHARS: usize = 40;

/// Maximum concurrent TTS synthesis requests per voice turn. Frames are
/// still emitted strictly in sentence order.
pub const TTS_MAX_IN_FLIGHT: usize = 2;

// ── Image data-URI validation ────────────────────────────────────

const ALLOWED_IMAGE_DATA_URI_PREFIXES: &[&str] = &[
    "data:image/png;base64,",
    "data:image/jpeg;base64,",
    "data:image/webp;base64,",
];

/// Validate a client-supplied image data URI: allowed MIME types only
/// (png/jpeg/webp, base64), non-empty payload, and an estimated decoded
/// size within [`MAX_IMAGE_BYTES`].
pub fn validate_image_data_uri(uri: &str) -> Result<(), String> {
    let payload = ALLOWED_IMAGE_DATA_URI_PREFIXES
        .iter()
        .find_map(|prefix| uri.strip_prefix(prefix))
        .ok_or_else(|| {
            "images must be base64 data URIs of type image/png, image/jpeg, or image/webp"
                .to_string()
        })?;
    if payload.is_empty() {
        return Err("image data URI has an empty payload".to_string());
    }
    // Estimate the decoded size without decoding: 3 bytes per 4 base64 chars.
    let estimated_bytes = payload.len() / 4 * 3;
    if estimated_bytes > MAX_IMAGE_BYTES {
        return Err(format!(
            "image exceeds the {} MB per-image limit",
            MAX_IMAGE_BYTES / (1024 * 1024)
        ));
    }
    Ok(())
}

/// Validate `images` and prepend `[IMAGE:<uri>] ` markers (the multimodal
/// wire form consumed by `zeroclaw_providers::multimodal`) to `content`.
pub fn prepend_image_markers(content: &str, images: &[String]) -> Result<String, String> {
    if images.is_empty() {
        return Ok(content.to_string());
    }
    if images.len() > MAX_IMAGES_PER_TURN {
        return Err(format!(
            "too many images: {} (max {MAX_IMAGES_PER_TURN})",
            images.len()
        ));
    }
    let mut out =
        String::with_capacity(content.len() + images.iter().map(|i| i.len() + 10).sum::<usize>());
    for uri in images {
        validate_image_data_uri(uri)?;
        out.push_str("[IMAGE:");
        out.push_str(uri);
        out.push_str("] ");
    }
    out.push_str(content);
    Ok(out)
}

// ── Resolved TTS binding for a voice turn ────────────────────────

/// The TTS provider binding a voice turn synthesizes through: the owning
/// agent's `tts_provider` when configured, else the install's first
/// configured provider.
pub struct TtsForward {
    pub manager: Arc<TtsManager>,
    /// Dotted provider alias (`<type>.<alias>`).
    pub provider_alias: String,
    /// Voice identifier passed to the provider.
    pub voice: String,
    /// Audio container reported in `tts_chunk` frames (`mp3`/`wav`/`opus`).
    pub format: String,
}

impl TtsForward {
    /// Resolve the TTS binding for a voice turn run by `agent_alias`.
    /// Returns `None` (logged) when no TTS provider is configured — the
    /// voice turn then streams text frames only.
    pub fn resolve(config: &Config, agent_alias: &str) -> Option<Self> {
        let manager = match TtsManager::from_config_for_agent(config, Some(agent_alias)) {
            Ok(manager) => manager,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "agent": agent_alias,
                            "error": format!("{e:#}"),
                        })),
                    "voice turn TTS manager construction failed; streaming text only"
                );
                return None;
            }
        };
        let Some(provider_alias) = manager.resolve_voice_provider() else {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"agent": agent_alias})),
                "voice turn without TTS: no TTS provider configured"
            );
            return None;
        };
        let voice = manager.voice_for_provider(&provider_alias).to_string();
        let format = manager
            .output_format_for_provider(&provider_alias)
            .unwrap_or("mp3")
            .to_string();
        Some(Self {
            manager: Arc::new(manager),
            provider_alias,
            voice,
            format,
        })
    }
}

// ── Sentence chunker ─────────────────────────────────────────────

/// Streaming sentence chunker for TTS: feed assistant text deltas with
/// [`push`](SentenceChunker::push), collect speakable units at sentence
/// boundaries (`.` `!` `?` `…` followed by whitespace, or a newline).
/// Units shorter than the configured minimum are merged with the next
/// sentence; [`flush`](SentenceChunker::flush) drains the remainder at
/// turn end. Fenced code blocks are dropped (not spoken) and each emitted
/// unit is passed through [`strip_markdown_for_speech`].
pub struct SentenceChunker {
    min_chars: usize,
    buf: String,
    /// Backticks accumulated at the start of the current line — a
    /// candidate code-fence opener/closer held back until disambiguated.
    line_start_ticks: usize,
    at_line_start: bool,
    in_fence: bool,
    /// Units emitted so far this turn. The FIRST unit skips the
    /// minimum-length merge: time-to-first-audio beats prosody, so a short
    /// opener like "Sure!" starts synthesizing immediately instead of
    /// waiting to be merged with the next sentence.
    emitted: usize,
}

impl Default for SentenceChunker {
    fn default() -> Self {
        Self::new()
    }
}

impl SentenceChunker {
    pub fn new() -> Self {
        Self::with_min_chars(DEFAULT_MIN_TTS_UNIT_CHARS)
    }

    pub fn with_min_chars(min_chars: usize) -> Self {
        Self {
            min_chars,
            buf: String::new(),
            line_start_ticks: 0,
            at_line_start: true,
            in_fence: false,
            emitted: 0,
        }
    }

    /// Feed a streamed text delta; returns zero or more speakable units
    /// (markdown-stripped, in order) completed by this delta.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        let mut out = Vec::new();
        for ch in delta.chars() {
            self.feed_char(ch, &mut out);
        }
        out
    }

    /// Drain the remaining buffered text as a final unit (markdown
    /// stripped), regardless of the minimum-length threshold.
    pub fn flush(&mut self) -> Option<String> {
        // Pending line-start backticks were not a fence opener after all.
        self.flush_pending_ticks();
        let unit = strip_markdown_for_speech(&self.buf);
        self.buf.clear();
        self.at_line_start = true;
        if unit.is_empty() { None } else { Some(unit) }
    }

    fn flush_pending_ticks(&mut self) {
        for _ in 0..self.line_start_ticks {
            self.buf.push('`');
        }
        self.line_start_ticks = 0;
    }

    fn feed_char(&mut self, ch: char, out: &mut Vec<String>) {
        if self.in_fence {
            // Inside a fenced code block: drop everything, watching for a
            // closing line that starts with ```.
            if self.at_line_start && ch == '`' {
                self.line_start_ticks += 1;
                if self.line_start_ticks == 3 {
                    self.in_fence = false;
                    self.line_start_ticks = 0;
                    self.at_line_start = false;
                }
                return;
            }
            self.line_start_ticks = 0;
            self.at_line_start = ch == '\n';
            return;
        }

        // A run of backticks at the start of a line may open a fence.
        if ch == '`' && (self.at_line_start || self.line_start_ticks > 0) {
            self.line_start_ticks += 1;
            self.at_line_start = false;
            if self.line_start_ticks == 3 {
                self.in_fence = true;
                self.line_start_ticks = 0;
                // Emit whatever prose is already complete before the code
                // block so it isn't held hostage by a long fence.
                self.emit_if_ready(true, out);
            }
            return;
        }
        if self.line_start_ticks > 0 {
            // Not a fence — restore the held backticks as inline code.
            self.flush_pending_ticks();
        }

        if ch == '\n' {
            self.at_line_start = true;
            if !self.emit_if_ready(false, out) {
                // Too short: keep the newline so per-line markdown
                // stripping still sees line starts, and merge onward.
                self.buf.push('\n');
            }
            return;
        }

        self.at_line_start = false;
        if ch.is_whitespace() && ends_at_sentence_boundary(&self.buf) {
            if !self.emit_if_ready(false, out) {
                self.buf.push(ch);
            }
            return;
        }
        self.buf.push(ch);
    }

    /// Emit the buffer as a unit when it satisfies the minimum length (or
    /// unconditionally when `force`). Returns true when the buffer was
    /// consumed (even if stripping produced nothing speakable).
    fn emit_if_ready(&mut self, force: bool, out: &mut Vec<String>) -> bool {
        let trimmed_chars = self.buf.trim().chars().count();
        if trimmed_chars == 0 {
            self.buf.clear();
            return true;
        }
        // First-audio fast path: the turn's opening sentence ships as soon
        // as its boundary arrives, however short — every millisecond before
        // the companion starts speaking is dead air.
        let min = if self.emitted == 0 { 1 } else { self.min_chars };
        if !force && trimmed_chars < min {
            return false;
        }
        let unit = strip_markdown_for_speech(&self.buf);
        self.buf.clear();
        if !unit.is_empty() {
            out.push(unit);
            self.emitted += 1;
        }
        true
    }
}

/// Spoken narration for a tool call on a voice turn, used when the model
/// itself went silent before acting (the soul asks it to narrate; this is
/// the guarantee). Short, present-tense, no jargon — it's said aloud.
#[must_use]
pub fn tool_narration(tool_name: &str) -> &'static str {
    match tool_name {
        "web_search" | "web_search_tool" => "Searching the web.",
        "web_fetch" | "text_browser" => "Reading that page.",
        "browser" | "browser_open" | "browser_delegate" => "Using the browser.",
        "shell" => "Running a command.",
        "file_read" | "glob_search" | "content_search" => "Looking through files.",
        "file_write" | "file_edit" => "Writing that down.",
        "memory_store" => "Making a note of that.",
        "memory_recall" => "Checking my memory.",
        "screenshot" => "Taking a look at the screen.",
        "cron_add" | "schedule" => "Setting that up on a schedule.",
        "http_request" => "Calling that service.",
        "image_gen" => "Drawing that up.",
        "delegate" | "spawn_subagent" => "Handing that to a helper.",
        _ => "Working on it.",
    }
}

/// Minimum quiet time between synthesized tool narrations, so a rapid burst
/// of tool calls yields one spoken line, not a stutter of them.
pub const TOOL_NARRATION_MIN_GAP_MS: u128 = 2500;

fn ends_at_sentence_boundary(buf: &str) -> bool {
    const TERMINATORS: [char; 4] = ['.', '!', '?', '…'];
    let mut rev = buf.chars().rev();
    let Some(last) = rev.next() else { return false };
    if TERMINATORS.contains(&last) {
        return true;
    }
    // Allow a closing quote/bracket between the terminator and the space:
    // `He said "stop." Then…`
    if matches!(last, '"' | '\'' | ')' | ']' | '\u{201d}' | '\u{2019}') {
        if let Some(prev) = rev.next() {
            return TERMINATORS.contains(&prev);
        }
    }
    false
}

// ── Markdown stripping for speech ────────────────────────────────

/// Strip markdown so a unit reads naturally when spoken: fenced code
/// blocks are dropped, headers/blockquote/bullet prefixes removed, links
/// and images reduced to their text, and `*`/`` ` `` emphasis markers
/// removed. Whitespace is collapsed to single spaces.
pub fn strip_markdown_for_speech(text: &str) -> String {
    let mut spoken = String::with_capacity(text.len());
    let mut in_fence = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let without_prefix = strip_line_prefixes(trimmed);
        let without_links = strip_links(without_prefix);
        let cleaned: String = without_links
            .chars()
            .filter(|c| *c != '*' && *c != '`')
            .collect();
        if !cleaned.trim().is_empty() {
            if !spoken.is_empty() {
                spoken.push(' ');
            }
            spoken.push_str(&cleaned);
        }
    }
    // Collapse whitespace runs and trim.
    spoken.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Remove leading header (`#`), blockquote (`>`), and bullet (`- `, `* `,
/// `+ `) markers from a line.
fn strip_line_prefixes(line: &str) -> &str {
    let mut rest = line;
    // Blockquotes can nest (`> > text`).
    while let Some(stripped) = rest.strip_prefix('>') {
        rest = stripped.trim_start();
    }
    let hashes = rest.chars().take_while(|c| *c == '#').count();
    if hashes > 0 {
        let after = &rest[hashes..];
        if after.is_empty() {
            return "";
        }
        if let Some(stripped) = after.strip_prefix(' ') {
            rest = stripped.trim_start();
        }
    }
    for bullet in ["- ", "* ", "+ "] {
        if let Some(stripped) = rest.strip_prefix(bullet) {
            return stripped;
        }
    }
    rest
}

/// Replace `[text](url)` / `![alt](url)` with `text` / `alt`.
fn strip_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let (marker_len, is_link_open) = if rest.starts_with("![") {
            (2, true)
        } else if rest.starts_with('[') {
            (1, true)
        } else {
            (0, false)
        };
        if is_link_open {
            let label_start = i + marker_len;
            if let Some(close_rel) = text[label_start..].find(']') {
                let close = label_start + close_rel;
                let after_close = &text[close + 1..];
                if after_close.starts_with('(') {
                    if let Some(paren_rel) = after_close.find(')') {
                        out.push_str(&text[label_start..close]);
                        i = close + 1 + paren_rel + 1;
                        continue;
                    }
                }
            }
        }
        let Some(ch) = rest.chars().next() else { break };
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

// ── Inline mascot control-tag parser ─────────────────────────────
//
// On voice turns the model may embed control tags between sentences to
// drive the mascot: `[emotion]` (a bare valid-emotion word) or
// `[gesture:name]`. These are stripped from BOTH captions and TTS text and
// surfaced as `mascot_cue` frames. Bare bracketed words that are not a
// known emotion/gesture pass through unchanged (they may be real prose like
// "[1, 2, 3]"). Parsing is incremental: a tag split across streamed deltas
// still parses, and a `[` with no closing `]` within
// [`MAX_CONTROL_TAG_LEN`] chars is flushed back out as literal text.

/// Emotions the mascot understands (contract C). A bare `[<emotion>]` tag
/// emits an emotion cue.
pub const VALID_EMOTIONS: &[&str] = &[
    "neutral",
    "happy",
    "excited",
    "curious",
    "thinking",
    "sleepy",
    "sad",
    "love",
    "proud",
    "mischievous",
    "focused",
    "surprised",
];

/// Gestures the mascot understands (contract C). `[gesture:<name>]` (or a
/// bare `[<gesture>]`) emits a gesture cue.
pub const VALID_GESTURES: &[&str] = &[
    "wave", "nod", "shakeHead", "shrug", "cheer", "celebrate", "laugh", "giggle", "think",
    "ponder", "idea", "heartEyes", "starEyes", "wink", "dance", "facepalm", "gasp", "surprise",
    "point", "bow", "hop", "spin",
];

/// Give up buffering a candidate tag once the run since `[` reaches this
/// many chars without a closing `]`; the buffer is flushed as literal text.
pub const MAX_CONTROL_TAG_LEN: usize = 40;

fn list_contains(list: &[&str], name: &str) -> bool {
    list.contains(&name)
}

/// Ordered output of [`ControlTagParser::push`]: interleaved cleaned text
/// and parsed cues, in stream order (so the caller can attribute a cue to
/// the sentence unit that follows it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlEvent {
    /// Tag-stripped assistant text (markdown preserved for captions).
    Text(String),
    /// A parsed control tag; exactly one of `emotion`/`gesture` is set.
    Cue {
        emotion: Option<String>,
        gesture: Option<String>,
    },
}

enum TagClass {
    Emotion(String),
    Gesture(String),
    /// Recognized control-tag syntax with an unknown name — stripped, no cue.
    StrippedNoCue,
    /// Not a control tag — pass through unchanged.
    NotATag,
}

fn classify_tag(inner: &str) -> TagClass {
    let trimmed = inner.trim();
    if let Some(name) = trimmed.strip_prefix("gesture:") {
        let name = name.trim();
        return if list_contains(VALID_GESTURES, name) {
            TagClass::Gesture(name.to_string())
        } else {
            TagClass::StrippedNoCue
        };
    }
    if list_contains(VALID_EMOTIONS, trimmed) {
        return TagClass::Emotion(trimmed.to_string());
    }
    if list_contains(VALID_GESTURES, trimmed) {
        return TagClass::Gesture(trimmed.to_string());
    }
    TagClass::NotATag
}

/// Incremental control-tag stripper. Feed streamed deltas with
/// [`push`](ControlTagParser::push); drain the tail with
/// [`flush`](ControlTagParser::flush).
pub struct ControlTagParser {
    /// A candidate tag buffered since the last unmatched `[` (includes the
    /// leading `[`). `None` when not inside a bracket.
    tag_buf: Option<String>,
    in_fence: bool,
    at_line_start: bool,
    line_ticks: usize,
}

impl Default for ControlTagParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlTagParser {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tag_buf: None,
            in_fence: false,
            at_line_start: true,
            line_ticks: 0,
        }
    }

    /// Feed a streamed delta; returns ordered [`ControlEvent`]s. Text held
    /// back inside an unresolved bracket is not emitted until it resolves
    /// (or [`flush`](Self::flush) is called).
    pub fn push(&mut self, delta: &str) -> Vec<ControlEvent> {
        let mut events = Vec::new();
        let mut text = String::new();
        for ch in delta.chars() {
            self.feed_char(ch, &mut text, &mut events);
        }
        if !text.is_empty() {
            events.push(ControlEvent::Text(text));
        }
        events
    }

    /// Drain any half-buffered bracket as literal text at turn end.
    pub fn flush(&mut self) -> Vec<ControlEvent> {
        if let Some(buf) = self.tag_buf.take() {
            return vec![ControlEvent::Text(buf)];
        }
        Vec::new()
    }

    fn feed_char(&mut self, ch: char, text: &mut String, events: &mut Vec<ControlEvent>) {
        if self.tag_buf.is_some() {
            self.feed_tag_char(ch, text, events);
            return;
        }
        if ch == '\n' {
            text.push('\n');
            self.at_line_start = true;
            self.line_ticks = 0;
            return;
        }
        if ch == '`' {
            if self.at_line_start || self.line_ticks > 0 {
                self.line_ticks += 1;
            }
            text.push('`');
            if self.line_ticks == 3 {
                self.in_fence = !self.in_fence;
                self.line_ticks = 0;
            }
            self.at_line_start = false;
            return;
        }
        self.line_ticks = 0;
        self.at_line_start = false;
        if !self.in_fence && ch == '[' {
            self.tag_buf = Some(String::from('['));
            return;
        }
        text.push(ch);
    }

    fn feed_tag_char(&mut self, ch: char, text: &mut String, events: &mut Vec<ControlEvent>) {
        match ch {
            ']' => {
                let buf = self.tag_buf.take().expect("tag_buf is Some");
                let inner = &buf[1..];
                match classify_tag(inner) {
                    TagClass::Emotion(e) => flush_text_then_cue(text, events, Some(e), None),
                    TagClass::Gesture(g) => flush_text_then_cue(text, events, None, Some(g)),
                    TagClass::StrippedNoCue => {}
                    TagClass::NotATag => {
                        text.push('[');
                        text.push_str(inner);
                        text.push(']');
                    }
                }
            }
            '[' => {
                // The prior partial wasn't a tag; flush it and restart.
                let prev = self.tag_buf.replace(String::from('[')).expect("tag_buf is Some");
                text.push_str(&prev);
            }
            '\n' => {
                // Tags never span lines; the partial was literal text.
                let prev = self.tag_buf.take().expect("tag_buf is Some");
                text.push_str(&prev);
                text.push('\n');
                self.at_line_start = true;
                self.line_ticks = 0;
            }
            _ => {
                let buf = self.tag_buf.as_mut().expect("tag_buf is Some");
                buf.push(ch);
                if buf.chars().count() >= MAX_CONTROL_TAG_LEN {
                    let prev = self.tag_buf.take().expect("tag_buf is Some");
                    text.push_str(&prev);
                }
            }
        }
    }
}

fn flush_text_then_cue(
    text: &mut String,
    events: &mut Vec<ControlEvent>,
    emotion: Option<String>,
    gesture: Option<String>,
) {
    if !text.is_empty() {
        events.push(ControlEvent::Text(std::mem::take(text)));
    }
    events.push(ControlEvent::Cue { emotion, gesture });
}

/// A mascot cue attributed to a sentence-unit index (contract B). The cue
/// applies to the unit that follows the tag; a trailing tag is clamped to
/// the last emitted unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MascotCue {
    pub seq: u64,
    pub emotion: Option<String>,
    pub gesture: Option<String>,
}

struct PendingCue {
    target: u64,
    emotion: Option<String>,
    gesture: Option<String>,
}

/// Per-delta output of [`VoiceTurnRouter`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RouterOutput {
    /// Tag-stripped caption text for the `chunk` frame (markdown preserved).
    pub caption: String,
    /// Completed sentence units with their assigned seq (== unit ordinal).
    pub units: Vec<(u64, String)>,
    /// Cues released this delta, each attributed to a sentence-unit index.
    pub cues: Vec<MascotCue>,
}

/// Drives a voice turn's text side: strips control tags, splits into TTS
/// sentence units, and attributes each cue to the seq of the sentence unit
/// that follows its tag. `seq` here is the sentence-unit ordinal — the same
/// numbering the HTTP TTS path stamps onto its `tts_chunk` frames.
pub struct VoiceTurnRouter {
    parser: ControlTagParser,
    chunker: SentenceChunker,
    next_seq: u64,
    pending: Vec<PendingCue>,
}

impl Default for VoiceTurnRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl VoiceTurnRouter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            parser: ControlTagParser::new(),
            chunker: SentenceChunker::new(),
            next_seq: 0,
            pending: Vec::new(),
        }
    }

    /// Emit `text` as its own TTS unit RIGHT NOW, without disturbing any
    /// partially buffered model sentence (which keeps accumulating and will
    /// emit after this unit, in order). Used for server-side tool narration
    /// on voice turns — the spoken "Searching the web." while the model has
    /// gone quiet to act. Returns the unit with its assigned seq.
    pub fn inject_unit(&mut self, text: &str) -> (u64, String) {
        let seq = self.next_seq;
        self.next_seq += 1;
        (seq, text.to_string())
    }

    /// Feed a streamed assistant delta.
    pub fn push(&mut self, delta: &str) -> RouterOutput {
        let mut out = RouterOutput::default();
        for event in self.parser.push(delta) {
            self.consume(event, &mut out);
        }
        out
    }

    /// Drain the tail at turn end: the final sentence unit, any leftover
    /// caption text, and trailing cues clamped to the last unit.
    pub fn flush(&mut self) -> RouterOutput {
        let mut out = RouterOutput::default();
        for event in self.parser.flush() {
            self.consume(event, &mut out);
        }
        if let Some(unit) = self.chunker.flush() {
            self.emit_unit(unit, &mut out);
        }
        if !self.pending.is_empty() {
            // Contract B: a trailing tag uses the last seq.
            let last = self.next_seq.saturating_sub(1);
            for cue in self.pending.drain(..) {
                out.cues.push(MascotCue {
                    seq: last,
                    emotion: cue.emotion,
                    gesture: cue.gesture,
                });
            }
        }
        out
    }

    fn consume(&mut self, event: ControlEvent, out: &mut RouterOutput) {
        match event {
            ControlEvent::Text(t) => {
                out.caption.push_str(&t);
                for unit in self.chunker.push(&t) {
                    self.emit_unit(unit, out);
                }
            }
            ControlEvent::Cue { emotion, gesture } => {
                self.pending.push(PendingCue {
                    target: self.next_seq,
                    emotion,
                    gesture,
                });
            }
        }
    }

    fn emit_unit(&mut self, unit: String, out: &mut RouterOutput) {
        let seq = self.next_seq;
        let mut i = 0;
        while i < self.pending.len() {
            if self.pending[i].target == seq {
                let cue = self.pending.remove(i);
                out.cues.push(MascotCue {
                    seq,
                    emotion: cue.emotion,
                    gesture: cue.gesture,
                });
            } else {
                i += 1;
            }
        }
        out.units.push((seq, unit));
        self.next_seq += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_all(chunker: &mut SentenceChunker, text: &str) -> Vec<String> {
        // Feed in tiny deltas to exercise cross-delta boundaries.
        let mut units = Vec::new();
        for chunk in text.as_bytes().chunks(3) {
            let s = std::str::from_utf8(chunk).unwrap_or_default();
            units.extend(chunker.push(s));
        }
        units
    }

    // ── Sentence boundaries ──────────────────────────────────────

    #[test]
    fn chunker_emits_at_sentence_boundary_over_min_length() {
        let mut chunker = SentenceChunker::with_min_chars(10);
        let units = chunker.push("This is a full sentence. And the tail continues");
        assert_eq!(units, vec!["This is a full sentence."]);
        assert_eq!(chunker.flush().as_deref(), Some("And the tail continues"));
    }

    #[test]
    fn chunker_handles_question_exclamation_and_ellipsis() {
        let mut chunker = SentenceChunker::with_min_chars(5);
        let units = chunker.push("Is this working? Absolutely correct! Wait for it… Done now. ");
        assert_eq!(
            units,
            vec![
                "Is this working?",
                "Absolutely correct!",
                "Wait for it…",
                "Done now."
            ]
        );
    }

    #[test]
    fn chunker_newline_is_a_boundary() {
        let mut chunker = SentenceChunker::with_min_chars(5);
        let units = chunker.push("First line without terminator\nSecond line");
        assert_eq!(units, vec!["First line without terminator"]);
        assert_eq!(chunker.flush().as_deref(), Some("Second line"));
    }

    #[test]
    fn chunker_boundary_split_across_deltas() {
        let mut chunker = SentenceChunker::with_min_chars(5);
        let mut units = chunker.push("A sentence that ends here.");
        assert!(units.is_empty(), "no trailing whitespace seen yet");
        units.extend(chunker.push(" next"));
        assert_eq!(units, vec!["A sentence that ends here."]);
    }

    #[test]
    fn chunker_does_not_split_inside_decimal_numbers() {
        let mut chunker = SentenceChunker::with_min_chars(5);
        // "3.14" — the period is not followed by whitespace, so no boundary.
        let units = chunker.push("The value of pi is 3.14159 which is neat. And more");
        assert_eq!(units, vec!["The value of pi is 3.14159 which is neat."]);
    }

    #[test]
    fn chunker_allows_closing_quote_after_terminator() {
        let mut chunker = SentenceChunker::with_min_chars(5);
        let units = chunker.push("He said \"stop.\" Then he left. ");
        assert_eq!(units, vec!["He said \"stop.\"", "Then he left."]);
    }

    // ── Minimum-length merging ───────────────────────────────────

    #[test]
    fn chunker_merges_tiny_sentences_up_to_min_length() {
        // The FIRST unit ships at its sentence boundary regardless of length
        // (time-to-first-audio fast path); the min-length merge applies from
        // the second unit onward.
        let mut chunker = SentenceChunker::new(); // min 40
        let units = push_all(
            &mut chunker,
            "Yes. OK. This is now a much longer sentence that crosses the threshold. Tail",
        );
        assert_eq!(
            units,
            vec![
                "Yes.",
                "OK. This is now a much longer sentence that crosses the threshold."
            ]
        );
        assert_eq!(chunker.flush().as_deref(), Some("Tail"));
    }

    #[test]
    fn chunker_first_unit_ships_immediately_even_when_short() {
        let mut chunker = SentenceChunker::new(); // min 40
        assert_eq!(push_all(&mut chunker, "Sure! Let me lo"), vec!["Sure!"]);
        // Second unit still merges up to the threshold.
        assert_eq!(push_all(&mut chunker, "ok. And"), Vec::<String>::new());
    }

    #[test]
    fn chunker_flush_emits_short_remainder() {
        let mut chunker = SentenceChunker::new();
        assert!(chunker.push("Hi.").is_empty());
        assert_eq!(chunker.flush().as_deref(), Some("Hi."));
        assert_eq!(chunker.flush(), None, "flush is idempotent once drained");
    }

    // ── Markdown stripping ───────────────────────────────────────

    #[test]
    fn chunker_drops_fenced_code_blocks() {
        let mut chunker = SentenceChunker::with_min_chars(5);
        let mut units = push_all(
            &mut chunker,
            "Here is the fix explained clearly.\n```rust\nlet x = 1. Run it! ??\n```\nAfter the code block. ",
        );
        units.extend(chunker.flush());
        assert_eq!(
            units,
            vec![
                "Here is the fix explained clearly.",
                "After the code block."
            ]
        );
    }

    #[test]
    fn chunker_keeps_inline_backticks_as_text() {
        let mut chunker = SentenceChunker::with_min_chars(5);
        let mut units = chunker.push("`cargo` is the tool we use here. ");
        units.extend(chunker.flush());
        assert_eq!(units, vec!["cargo is the tool we use here."]);
    }

    #[test]
    fn strip_markdown_removes_emphasis_headers_and_bullets() {
        assert_eq!(
            strip_markdown_for_speech("## Heading\n- **bold** item\n> quoted *words*"),
            "Heading bold item quoted words"
        );
    }

    #[test]
    fn strip_markdown_keeps_link_text_only() {
        assert_eq!(
            strip_markdown_for_speech(
                "See [the docs](https://example.com/x) and ![alt text](img.png)."
            ),
            "See the docs and alt text."
        );
    }

    #[test]
    fn strip_markdown_leaves_plain_brackets_alone() {
        assert_eq!(
            strip_markdown_for_speech("Arrays like [1, 2, 3] stay intact"),
            "Arrays like [1, 2, 3] stay intact"
        );
    }

    #[test]
    fn strip_markdown_drops_fences_and_collapses_whitespace() {
        assert_eq!(
            strip_markdown_for_speech("before\n```py\nprint(1)\n```\nafter   words"),
            "before after words"
        );
    }

    // ── Image data-URI validation ────────────────────────────────

    #[test]
    fn data_uri_accepts_png_jpeg_webp() {
        for mime in ["png", "jpeg", "webp"] {
            let uri = format!("data:image/{mime};base64,AAAA");
            assert!(
                validate_image_data_uri(&uri).is_ok(),
                "expected {mime} to be accepted"
            );
        }
    }

    #[test]
    fn data_uri_rejects_other_schemes_and_mimes() {
        for uri in [
            "https://example.com/x.png",
            "data:image/gif;base64,AAAA",
            "data:image/svg+xml;base64,AAAA",
            "data:text/html;base64,AAAA",
            "file:///etc/passwd",
            "data:image/png;base64",
        ] {
            assert!(
                validate_image_data_uri(uri).is_err(),
                "expected {uri} to be rejected"
            );
        }
    }

    #[test]
    fn data_uri_rejects_empty_payload() {
        assert!(validate_image_data_uri("data:image/png;base64,").is_err());
    }

    #[test]
    fn data_uri_rejects_oversized_payload() {
        // Base64 payload whose decoded estimate exceeds 5 MB.
        let oversized = format!(
            "data:image/jpeg;base64,{}",
            "A".repeat(MAX_IMAGE_BYTES / 3 * 4 + 8)
        );
        let err = validate_image_data_uri(&oversized).unwrap_err();
        assert!(err.contains("per-image limit"), "got: {err}");
    }

    #[test]
    fn prepend_image_markers_builds_marker_prefix() {
        let images = vec!["data:image/png;base64,AAAA".to_string()];
        let content = prepend_image_markers("what is this?", &images).unwrap();
        assert_eq!(content, "[IMAGE:data:image/png;base64,AAAA] what is this?");
    }

    #[test]
    fn prepend_image_markers_caps_image_count() {
        let images: Vec<String> = (0..MAX_IMAGES_PER_TURN + 1)
            .map(|_| "data:image/png;base64,AAAA".to_string())
            .collect();
        let err = prepend_image_markers("hi", &images).unwrap_err();
        assert!(err.contains("too many images"), "got: {err}");
    }

    #[test]
    fn prepend_image_markers_passthrough_without_images() {
        assert_eq!(prepend_image_markers("hello", &[]).unwrap(), "hello");
    }

    // ── Control-tag parser ───────────────────────────────────────

    /// Feed `text` in tiny 3-byte deltas (exercising cross-delta buffering),
    /// then flush. Returns the reassembled cleaned text and the events.
    fn parse_all(text: &str) -> (String, Vec<ControlEvent>) {
        let mut parser = ControlTagParser::new();
        let mut events = Vec::new();
        for chunk in text.as_bytes().chunks(3) {
            let s = std::str::from_utf8(chunk).unwrap_or_default();
            events.extend(parser.push(s));
        }
        events.extend(parser.flush());
        let mut cleaned = String::new();
        for e in &events {
            if let ControlEvent::Text(t) = e {
                cleaned.push_str(t);
            }
        }
        (cleaned, events)
    }

    fn cue_pairs(events: &[ControlEvent]) -> Vec<(Option<String>, Option<String>)> {
        events
            .iter()
            .filter_map(|e| match e {
                ControlEvent::Cue { emotion, gesture } => Some((emotion.clone(), gesture.clone())),
                ControlEvent::Text(_) => None,
            })
            .collect()
    }

    #[test]
    fn parser_strips_bare_emotion_and_emits_cue() {
        let (text, events) = parse_all("[happy] Great news!");
        assert_eq!(text, " Great news!");
        assert_eq!(cue_pairs(&events), vec![(Some("happy".to_string()), None)]);
    }

    #[test]
    fn parser_strips_gesture_prefix_and_emits_cue() {
        let (text, events) = parse_all("Ready. [gesture:cheer] Go!");
        assert_eq!(text, "Ready.  Go!");
        assert_eq!(cue_pairs(&events), vec![(None, Some("cheer".to_string()))]);
    }

    #[test]
    fn parser_treats_bare_gesture_word_as_gesture_cue() {
        let (_text, events) = parse_all("[wave] hi");
        assert_eq!(cue_pairs(&events), vec![(None, Some("wave".to_string()))]);
    }

    #[test]
    fn parser_strips_unknown_gesture_without_cue() {
        let (text, events) = parse_all("Hi [gesture:foobar] there");
        assert_eq!(text, "Hi  there");
        assert!(cue_pairs(&events).is_empty());
    }

    #[test]
    fn parser_passes_through_unknown_bare_bracket() {
        let (text, events) = parse_all("Array [1, 2, 3] stays intact");
        assert_eq!(text, "Array [1, 2, 3] stays intact");
        assert!(cue_pairs(&events).is_empty());
    }

    #[test]
    fn parser_handles_tag_split_across_deltas() {
        let mut parser = ControlTagParser::new();
        let mut events = parser.push("Hello [ha");
        events.extend(parser.push("ppy] world"));
        let cleaned: String = events
            .iter()
            .filter_map(|e| match e {
                ControlEvent::Text(t) => Some(t.clone()),
                ControlEvent::Cue { .. } => None,
            })
            .collect();
        assert_eq!(cleaned, "Hello  world");
        assert_eq!(cue_pairs(&events), vec![(Some("happy".to_string()), None)]);
    }

    #[test]
    fn parser_exempts_code_fences() {
        let input = "text:\n```\n[happy] not a cue [gesture:wave]\n```\ndone";
        let (text, events) = parse_all(input);
        assert_eq!(text, input, "brackets inside a fence pass through verbatim");
        assert!(cue_pairs(&events).is_empty());
    }

    #[test]
    fn parser_flushes_unterminated_bracket_as_literal() {
        let (text, events) = parse_all("end [happy");
        assert_eq!(text, "end [happy");
        assert!(cue_pairs(&events).is_empty());
    }

    #[test]
    fn parser_flushes_overlong_bracket_run_as_literal() {
        let input = format!("[{}", "x".repeat(45));
        let (text, events) = parse_all(&input);
        assert_eq!(text, input, "a '[' with no ']' within 40 chars becomes literal");
        assert!(cue_pairs(&events).is_empty());
    }

    // ── Voice turn router (seq attribution) ──────────────────────

    fn route_all(text: &str) -> RouterOutput {
        let mut router = VoiceTurnRouter::new();
        let mut out = RouterOutput::default();
        for chunk in text.as_bytes().chunks(4) {
            let s = std::str::from_utf8(chunk).unwrap_or_default();
            let o = router.push(s);
            out.caption.push_str(&o.caption);
            out.units.extend(o.units);
            out.cues.extend(o.cues);
        }
        let o = router.flush();
        out.caption.push_str(&o.caption);
        out.units.extend(o.units);
        out.cues.extend(o.cues);
        out
    }

    #[test]
    fn router_attributes_cue_to_following_unit() {
        let out = route_all(
            "[happy] This is the first sufficiently long sentence here. \
             [gesture:cheer] And here comes the second long sentence now.",
        );
        let seqs: Vec<u64> = out.units.iter().map(|(s, _)| *s).collect();
        assert_eq!(seqs, vec![0, 1]);
        assert_eq!(
            out.cues,
            vec![
                MascotCue {
                    seq: 0,
                    emotion: Some("happy".to_string()),
                    gesture: None,
                },
                MascotCue {
                    seq: 1,
                    emotion: None,
                    gesture: Some("cheer".to_string()),
                },
            ]
        );
        assert!(!out.caption.contains('['));
        assert!(out.caption.contains("first sufficiently long sentence"));
    }

    #[test]
    fn router_clamps_trailing_tag_to_last_seq() {
        let out =
            route_all("This is a sufficiently long sentence to emit now. [gesture:bow]");
        assert_eq!(out.units.len(), 1);
        assert_eq!(
            out.cues,
            vec![MascotCue {
                seq: 0,
                emotion: None,
                gesture: Some("bow".to_string()),
            }]
        );
    }

    #[test]
    fn router_units_are_markdown_stripped() {
        let out = route_all("- **Bold** point that is quite long enough to emit as one unit.");
        assert_eq!(out.units.len(), 1);
        assert_eq!(
            out.units[0].1,
            "Bold point that is quite long enough to emit as one unit."
        );
    }
}
