//! Outbound media markers and the egress trust boundary.

use anyhow::Context as _;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use zeroclaw_runtime::i18n;

use super::embed::{DiscordEmbed, EmbedAuthor, EmbedField, EmbedFooter, EmbedMedia};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiscordAttachmentKind {
    Image,
    Document,
    Video,
    Audio,
    Voice,
}

impl DiscordAttachmentKind {
    fn from_marker(kind: &str) -> Option<Self> {
        match kind.trim().to_ascii_uppercase().as_str() {
            "IMAGE" | "PHOTO" => Some(Self::Image),
            "DOCUMENT" | "FILE" => Some(Self::Document),
            "VIDEO" => Some(Self::Video),
            "AUDIO" => Some(Self::Audio),
            "VOICE" => Some(Self::Voice),
            _ => None,
        }
    }

    pub(crate) fn marker_name(&self) -> &'static str {
        match self {
            Self::Image => "IMAGE",
            Self::Document => "DOCUMENT",
            Self::Video => "VIDEO",
            Self::Audio => "AUDIO",
            Self::Voice => "VOICE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscordAttachment {
    pub(crate) kind: DiscordAttachmentKind,
    pub(crate) target: String,
}

pub(crate) fn parse_attachment_markers(message: &str) -> (String, Vec<DiscordAttachment>) {
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
            let kind = DiscordAttachmentKind::from_marker(kind)?;
            let target = target.trim();
            if target.is_empty() {
                return None;
            }
            Some(DiscordAttachment {
                kind,
                target: target.to_string(),
            })
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

const EMBED_TAG: &str = "[EMBED:";

/// Author-supplied embed shape, deserialized from the `[EMBED:{json}]` payload.
/// Mirrors [`DiscordEmbed`] but takes bare URL strings for media and is lenient
/// about unknown keys (an agent typo drops the key, not the whole embed).
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub(crate) struct EmbedSpec {
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) url: Option<String>,
    #[serde(default)]
    pub(crate) color: Option<u32>,
    #[serde(default)]
    pub(crate) timestamp: Option<String>,
    #[serde(default)]
    pub(crate) footer: Option<EmbedFooterSpec>,
    #[serde(default)]
    pub(crate) image: Option<EmbedMediaSpec>,
    #[serde(default)]
    pub(crate) thumbnail: Option<EmbedMediaSpec>,
    #[serde(default)]
    pub(crate) author: Option<EmbedAuthorSpec>,
    #[serde(default)]
    pub(crate) fields: Vec<EmbedFieldSpec>,
}

/// An embed `image`/`thumbnail` value. Discord models these as objects
/// (`{ "url": … }`), which is what an agent following the "Discord embed JSON
/// object" affordance emits; a bare URL string is also accepted for leniency.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub(crate) enum EmbedMediaSpec {
    Url(String),
    Object { url: String },
}

impl EmbedMediaSpec {
    fn into_url(self) -> String {
        match self {
            Self::Url(url) => url,
            Self::Object { url } => url,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub(crate) struct EmbedFooterSpec {
    pub(crate) text: String,
    #[serde(default)]
    pub(crate) icon_url: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub(crate) struct EmbedAuthorSpec {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) url: Option<String>,
    #[serde(default)]
    pub(crate) icon_url: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub(crate) struct EmbedFieldSpec {
    pub(crate) name: String,
    pub(crate) value: String,
    #[serde(default)]
    pub(crate) inline: bool,
}

/// Parse `[EMBED:{json}]` markers out of `message`, returning the marker-free
/// text and the parsed specs in author order. A malformed marker (bad JSON,
/// missing closing `]`) is left verbatim so the author sees it failed rather
/// than having it silently vanish.
pub(crate) fn parse_embed_markers(message: &str) -> (String, Vec<EmbedSpec>) {
    let mut cleaned = String::with_capacity(message.len());
    let mut specs = Vec::new();
    let mut cursor = 0usize;

    while cursor < message.len() {
        let Some(rel) = find_ci(&message[cursor..], EMBED_TAG) else {
            break;
        };
        let tag_start = cursor + rel;
        match scan_one_embed(message, tag_start) {
            Some((end, Some(spec))) => {
                cleaned.push_str(&message[cursor..tag_start]);
                specs.push(spec);
                cursor = end;
            }
            Some((end, None)) => {
                // A structurally-complete `[EMBED:{…}]` whose JSON failed to
                // deserialize. Keep the whole span verbatim so the author sees
                // it failed, and skip PAST it — never re-scan inside the
                // rejected JSON (a nested `[EMBED:` there must not be parsed).
                cleaned.push_str(&message[cursor..end]);
                cursor = end;
            }
            None => {
                // Not a structural marker: keep the `[` literal and re-scan
                // from just past it.
                cleaned.push_str(&message[cursor..=tag_start]);
                cursor = tag_start + 1;
            }
        }
    }
    if cursor < message.len() {
        cleaned.push_str(&message[cursor..]);
    }
    (cleaned.trim().to_string(), specs)
}

fn scan_one_embed(message: &str, tag_start: usize) -> Option<(usize, Option<EmbedSpec>)> {
    let after_tag = tag_start + EMBED_TAG.len();
    let brace = next_non_ws(message, after_tag)?;
    if message.as_bytes().get(brace) != Some(&b'{') {
        return None;
    }
    let obj_end = json_object_end(message, brace)?;
    let close = next_non_ws(message, obj_end)?;
    if message.as_bytes().get(close) != Some(&b']') {
        return None;
    }
    let spec = serde_json::from_str::<EmbedSpec>(&message[brace..obj_end]).ok();
    Some((close + 1, spec))
}

/// Byte index of the next non-whitespace char at or after `from`.
fn next_non_ws(message: &str, from: usize) -> Option<usize> {
    message[from..]
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(i, _)| from + i)
}

/// Given `start` indexing a `{`, return the byte index just past the matching
/// `}`, honoring nested objects and JSON strings/escapes. `None` if unbalanced.
fn json_object_end(message: &str, start: usize) -> Option<usize> {
    let bytes = message.as_bytes();
    if bytes.get(start) != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, &c) in bytes[start..].iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
            continue;
        }
        match c {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(start + offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Case-insensitive (ASCII) substring search.
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

pub(crate) fn spec_to_embed(
    spec: EmbedSpec,
    workspace_dir: Option<&Path>,
) -> (Option<DiscordEmbed>, Vec<DiscordMarkerFailure>) {
    let mut failures = Vec::new();
    let mut vet = |url: Option<String>| -> Option<String> {
        let url = url?;
        match vet_embed_url(&url, workspace_dir) {
            Ok(url) => Some(url),
            Err(failure) => {
                failures.push(failure);
                None
            }
        }
    };

    let footer = spec.footer.map(|f| EmbedFooter {
        text: f.text,
        icon_url: vet(f.icon_url),
    });
    let author = spec.author.map(|a| EmbedAuthor {
        name: a.name,
        url: vet(a.url),
        icon_url: vet(a.icon_url),
    });
    let image = vet(spec.image.map(EmbedMediaSpec::into_url)).map(|url| EmbedMedia { url });
    let thumbnail = vet(spec.thumbnail.map(EmbedMediaSpec::into_url)).map(|url| EmbedMedia { url });
    let url = vet(spec.url);
    let fields = spec
        .fields
        .into_iter()
        .map(|f| EmbedField {
            name: f.name,
            value: f.value,
            inline: f.inline,
        })
        .collect();

    let embed = DiscordEmbed {
        title: spec.title,
        description: spec.description,
        url,
        color: spec.color,
        timestamp: spec.timestamp,
        footer,
        image,
        thumbnail,
        author,
        fields,
    };
    if embed.is_empty() {
        (None, failures)
    } else {
        (Some(embed), failures)
    }
}

/// Vet a single embed URL: accept only `http(s)` (Discord fetches/links it),
/// mapping a local-path or scheme rejection to a [`DiscordMarkerFailure`].
fn vet_embed_url(url: &str, workspace_dir: Option<&Path>) -> Result<String, DiscordMarkerFailure> {
    match validate_marker_target(url, workspace_dir) {
        Ok(DiscordMarkerTarget::Http(url)) => Ok(url),
        Ok(DiscordMarkerTarget::Local(_)) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(
                        ::serde_json::json!({ "url": url, "reason": "local_not_embeddable" })
                    ),
                "discord: embed URL is a local path; Discord cannot fetch local files for embeds"
            );
            Err(DiscordMarkerFailure::Refused)
        }
        Err(e) => Err(e.kind()),
    }
}

/// One component declared inside a `[COMPONENTS:{…}]` row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ComponentSpec {
    /// An action button: a click enqueues `prompt` as a new agent turn.
    Button {
        label: String,
        style: super::components::ButtonStyle,
        prompt: String,
    },
    /// A link button: opens `url`, never dispatched back to the bot.
    Link { label: String, url: String },
    /// A modal button: a click opens a text-input modal (Discord response
    /// type 9). The submitted field values are appended to `prompt`, which is
    /// then run as a new agent turn. The modal's own routing `custom_id` is
    /// minted at build time (it is the token the type-5 submit dispatches on).
    ModalButton {
        label: String,
        style: super::components::ButtonStyle,
        prompt: String,
        modal: ComponentModalSpec,
    },
    /// A string-select menu: choosing an option enqueues that option's `prompt`.
    Select {
        placeholder: String,
        options: Vec<ComponentOptionSpec>,
    },
}

/// The agent-declared shape of a modal opened by a [`ComponentSpec::ModalButton`].
/// Carries only the title + field declarations; the routing `custom_id` is
/// minted server-side at build time (never agent-supplied), so a click can't
/// alias another component's token.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ComponentModalSpec {
    pub(crate) title: String,
    pub(crate) fields: Vec<ComponentModalField>,
}

/// One text-input field declared inside a modal button's `modal.fields`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ComponentModalField {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) style: super::components::TextInputStyle,
    pub(crate) required: bool,
    pub(crate) placeholder: Option<String>,
    pub(crate) min_length: Option<u16>,
    pub(crate) max_length: Option<u16>,
}

/// One choice in a `[COMPONENTS:…]` select. `value` is the agent-supplied option
/// value (shown to no one); `prompt` is enqueued when the option is chosen.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ComponentOptionSpec {
    pub(crate) label: String,
    pub(crate) value: String,
    pub(crate) prompt: String,
}

/// Map a textual style name to a [`ButtonStyle`]. Unknown / missing → Secondary
/// (a neutral default rather than a parse failure that would drop the button).
fn button_style_from_str(s: Option<&str>) -> super::components::ButtonStyle {
    use super::components::ButtonStyle;
    match s.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("primary") => ButtonStyle::Primary,
        Some("success") => ButtonStyle::Success,
        Some("danger") => ButtonStyle::Danger,
        // "secondary" and anything unrecognized.
        _ => ButtonStyle::Secondary,
    }
}

/// Map a textual text-input style to a [`TextInputStyle`]. Unknown / missing →
/// Short (the common single-line default rather than a parse failure).
fn text_input_style_from_str(s: Option<&str>) -> super::components::TextInputStyle {
    use super::components::TextInputStyle;
    match s.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("paragraph") => TextInputStyle::Paragraph,
        // "short" and anything unrecognized.
        _ => TextInputStyle::Short,
    }
}

/// Read an optional `u16` length bound (`min`/`max`) from a modal-field object.
/// Out-of-range / non-numeric values are dropped (treated as absent) rather
/// than failing the field.
fn modal_field_len(field: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<u16> {
    field
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| u16::try_from(n).ok())
}

/// Parse a modal button's `modal` object into a [`ComponentModalSpec`]. `None`
/// when it has no renderable fields (each field requires `id` and `label`), so
/// the caller drops the whole button rather than rendering a modal that opens
/// empty.
fn modal_spec_from_json(v: &serde_json::Value) -> Option<ComponentModalSpec> {
    let obj = v.as_object()?;
    let title = obj
        .get("title")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let fields: Vec<ComponentModalField> = obj
        .get("fields")
        .and_then(|f| f.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|f| {
                    let fo = f.as_object()?;
                    let id = fo.get("id")?.as_str()?.to_string();
                    let label = fo.get("label")?.as_str()?.to_string();
                    if id.is_empty() || label.is_empty() {
                        return None;
                    }
                    Some(ComponentModalField {
                        id,
                        label,
                        style: text_input_style_from_str(fo.get("style").and_then(|x| x.as_str())),
                        required: fo
                            .get("required")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                        placeholder: fo
                            .get("placeholder")
                            .and_then(|x| x.as_str())
                            .filter(|p| !p.is_empty())
                            .map(ToString::to_string),
                        min_length: modal_field_len(fo, "min"),
                        max_length: modal_field_len(fo, "max"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if fields.is_empty() {
        return None;
    }
    Some(ComponentModalSpec { title, fields })
}

/// Parse one component JSON object into a [`ComponentSpec`]. `None` for a shape
/// that can't be rendered (e.g. a button with no label, a select with no
/// options) so the caller can skip it without failing the whole send.
fn component_from_json(v: &serde_json::Value) -> Option<ComponentSpec> {
    let obj = v.as_object()?;

    // Select: distinguished by the `select` key (its placeholder text).
    if let Some(placeholder) = obj.get("select") {
        let placeholder = placeholder.as_str().unwrap_or("").to_string();
        let options: Vec<ComponentOptionSpec> = obj
            .get("options")
            .and_then(|o| o.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|o| {
                        let oo = o.as_object()?;
                        let label = oo.get("label")?.as_str()?.to_string();
                        if label.is_empty() {
                            return None;
                        }
                        let value = oo
                            .get("value")
                            .and_then(|x| x.as_str())
                            .unwrap_or(label.as_str())
                            .to_string();
                        let prompt = oo
                            .get("prompt")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        Some(ComponentOptionSpec {
                            label,
                            value,
                            prompt,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        if options.is_empty() {
            return None;
        }
        return Some(ComponentSpec::Select {
            placeholder,
            options,
        });
    }

    // Buttons require a label.
    let label = obj.get("label").and_then(|x| x.as_str())?.to_string();
    if label.is_empty() {
        return None;
    }
    // Link button: distinguished by the `url` key.
    if let Some(url) = obj.get("url").and_then(|x| x.as_str()) {
        if url.is_empty() {
            return None;
        }
        return Some(ComponentSpec::Link {
            label,
            url: url.to_string(),
        });
    }
    // Modal button: distinguished by the `modal` key. A click opens a
    // text-input modal whose submitted values are appended to `prompt`.
    if let Some(modal_json) = obj.get("modal") {
        let modal = modal_spec_from_json(modal_json)?;
        let prompt = obj
            .get("prompt")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        return Some(ComponentSpec::ModalButton {
            label,
            style: button_style_from_str(obj.get("style").and_then(|x| x.as_str())),
            prompt,
            modal,
        });
    }
    // Action button.
    let prompt = obj
        .get("prompt")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    Some(ComponentSpec::Button {
        label,
        style: button_style_from_str(obj.get("style").and_then(|x| x.as_str())),
        prompt,
    })
}

/// Parse the `{"rows": [[…], …]}` body of a `[COMPONENTS:…]` marker into row
/// specs. Returns `None` when the JSON is malformed or carries no renderable
/// rows, so the caller drops the marker rather than 400-ing the send.
fn parse_components_body(json: &str) -> Option<Vec<Vec<ComponentSpec>>> {
    let parsed: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    let rows = parsed.get("rows")?.as_array()?;
    let rows: Vec<Vec<ComponentSpec>> = rows
        .iter()
        .filter_map(|row| {
            let comps: Vec<ComponentSpec> = row
                .as_array()?
                .iter()
                .filter_map(component_from_json)
                .collect();
            (!comps.is_empty()).then_some(comps)
        })
        .collect();
    (!rows.is_empty()).then_some(rows)
}

/// The marker tag, including the trailing colon — `[COMPONENTS:`.
const COMPONENTS_TAG: &str = "[COMPONENTS:";

pub(crate) fn parse_component_markers(message: &str) -> (String, Vec<Vec<ComponentSpec>>) {
    let mut cleaned = String::with_capacity(message.len());
    let mut rows: Vec<Vec<ComponentSpec>> = Vec::new();
    let mut rest = message;

    loop {
        let Some(tag_rel) = rest.find(COMPONENTS_TAG) else {
            // No further marker: keep the remaining text verbatim.
            cleaned.push_str(rest);
            break;
        };
        let body_start = tag_rel + COMPONENTS_TAG.len();
        match find_marker_extent(&rest[body_start..]) {
            Some((consumed, marker_rows)) => {
                // Recognised marker: drop it, keep the text before it.
                cleaned.push_str(&rest[..tag_rel]);
                rows.extend(marker_rows);
                rest = &rest[body_start + consumed..];
            }
            None => {
                cleaned.push_str(&rest[..body_start]);
                rest = &rest[body_start..];
            }
        }
    }

    (cleaned.trim().to_string(), rows)
}

fn strict_marker_close(after_tag: &str) -> Option<usize> {
    let mut depth: i64 = 0;
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in after_tag.char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' | '[' => depth += 1,
            '}' => depth -= 1,
            ']' => {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

fn repair_brackets(body: &str) -> String {
    let mut out = String::with_capacity(body.len() + 4);
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for c in body.chars() {
        if in_string {
            out.push(c);
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '{' | '[' => {
                stack.push(c);
                out.push(c);
            }
            '}' | ']' => loop {
                match stack.last().copied() {
                    // Extra closer with nothing open — drop it.
                    None => break,
                    Some(top) => {
                        let want = if top == '{' { '}' } else { ']' };
                        if c == want {
                            stack.pop();
                            out.push(c);
                            break;
                        }
                        // Dropped/mis-ordered closer: insert the wanted one, pop,
                        // and retry the current closer against the new top.
                        out.push(want);
                        stack.pop();
                    }
                }
            },
            _ => out.push(c),
        }
    }
    // Close anything still open (a dropped trailing closer).
    while let Some(top) = stack.pop() {
        out.push(if top == '{' { '}' } else { ']' });
    }
    out
}

fn find_marker_extent(after_tag: &str) -> Option<(usize, Vec<Vec<ComponentSpec>>)> {
    if let Some(close) = strict_marker_close(after_tag) {
        let body = &after_tag[..close];
        if serde_json::from_str::<serde_json::Value>(body.trim()).is_ok() {
            let rows = parse_components_body(body).unwrap_or_default();
            return Some((close + 1, rows)); // `]` is one ASCII byte
        }
    }
    // Case 2 — under-closed body: repair ONLY a trailing, single, parseable marker.
    let last_close = after_tag.rfind(']')?;
    if !after_tag[last_close + 1..].chars().all(char::is_whitespace) {
        return None; // prose follows the candidate close — refuse to consume it
    }
    let candidate = &after_tag[..last_close];
    if candidate.contains(COMPONENTS_TAG) {
        return None; // a later marker sits inside — don't repair across markers
    }
    let rows = parse_components_body(&repair_brackets(candidate))?; // must parse
    Some((last_close + 1, rows))
}

/// Resolved outbound attachment target after sandbox validation.
#[derive(Debug)]
pub(crate) enum DiscordMarkerTarget {
    Local(PathBuf),
    Http(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiscordMarkerFailure {
    /// Trust-boundary refusal: disallowed scheme, relative path, missing
    /// workspace_dir, or canonicalised path outside the workspace.
    Refused,
    /// Path passed scheme/absolute/workspace checks but did not resolve
    /// to anything on disk.
    NotFound,
}

#[derive(Debug)]
pub(crate) enum DiscordMarkerError {
    Refused(anyhow::Error),
    NotFound(anyhow::Error),
}

impl DiscordMarkerError {
    pub(crate) fn kind(&self) -> DiscordMarkerFailure {
        match self {
            Self::Refused(_) => DiscordMarkerFailure::Refused,
            Self::NotFound(_) => DiscordMarkerFailure::NotFound,
        }
    }
}

impl std::fmt::Display for DiscordMarkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(e) | Self::NotFound(e) => write!(f, "{e}"),
        }
    }
}

pub(crate) fn validate_marker_target(
    target: &str,
    workspace_dir: Option<&Path>,
) -> Result<DiscordMarkerTarget, DiscordMarkerError> {
    if target.starts_with("http://") || target.starts_with("https://") {
        return Ok(DiscordMarkerTarget::Http(target.to_string()));
    }
    if target.contains("://") {
        let scheme = target.split("://").next().unwrap_or("?");
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "scheme": scheme,
                    "target": target,
                })),
            "discord: marker target uses disallowed scheme"
        );
        return Err(DiscordMarkerError::Refused(anyhow::Error::msg(format!(
            "marker target uses disallowed scheme {scheme:?}; only http/https and absolute workspace paths are accepted"
        ))));
    }
    if target.starts_with("data:") || target.starts_with("file:") {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"target": target})),
            "discord: marker target uses disallowed data: or file: scheme"
        );
        return Err(DiscordMarkerError::Refused(anyhow::Error::msg(
            "marker target uses disallowed scheme; only http/https and absolute workspace paths are accepted",
        )));
    }

    let target_path = Path::new(target);
    if !target_path.is_absolute() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "target": target,
                    "reason": "not_absolute",
                })),
            "discord: marker target is not absolute"
        );
        return Err(DiscordMarkerError::Refused(anyhow::Error::msg(format!(
            "marker target {target} is not an absolute path; the agent must emit absolute paths inside workspace_dir"
        ))));
    }

    let workspace = workspace_dir.ok_or_else(|| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "target": target,
                    "reason": "no_workspace_dir",
                })),
            "discord: marker target is local path but channel has no workspace_dir"
        );
        DiscordMarkerError::Refused(anyhow::Error::msg(format!(
            "marker target {target} is a local path but the channel was started without a workspace_dir, refusing for safety"
        )))
    })?;
    let workspace_canon = std::fs::canonicalize(workspace)
        .with_context(|| format!("canonicalize workspace {}", workspace.display()))
        .map_err(DiscordMarkerError::Refused)?;
    let target_canon = match std::fs::canonicalize(target_path) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "target": target,
                        "reason": "not_found",
                    })),
                "discord: marker target not found on disk"
            );
            return Err(DiscordMarkerError::NotFound(anyhow::Error::msg(format!(
                "marker target {target} not found on disk"
            ))));
        }
        Err(e) => {
            return Err(DiscordMarkerError::Refused(
                anyhow::Error::from(e).context(format!("canonicalize marker target {target}")),
            ));
        }
    };

    if !target_canon.starts_with(&workspace_canon) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "target": target,
                    "target_canon": target_canon.display().to_string(),
                    "workspace_canon": workspace_canon.display().to_string(),
                    "reason": "outside_workspace",
                })),
            "discord: marker target escapes workspace_dir"
        );
        return Err(DiscordMarkerError::Refused(anyhow::Error::msg(format!(
            "marker target {target} resolves to {} which is outside workspace_dir {}; refusing",
            target_canon.display(),
            workspace_canon.display(),
        ))));
    }
    Ok(DiscordMarkerTarget::Local(target_canon))
}

pub(crate) fn classify_outgoing_attachments(
    attachments: &[DiscordAttachment],
    workspace_dir: Option<&Path>,
) -> (Vec<PathBuf>, Vec<String>, Vec<DiscordMarkerFailure>) {
    let mut local_files = Vec::new();
    let mut remote_urls = Vec::new();
    let mut failures = Vec::new();

    for attachment in attachments {
        match validate_marker_target(&attachment.target, workspace_dir) {
            Ok(DiscordMarkerTarget::Local(path)) => local_files.push(path),
            Ok(DiscordMarkerTarget::Http(url)) => remote_urls.push(url),
            Err(e) => {
                let kind_label = match e.kind() {
                    DiscordMarkerFailure::Refused => "trust boundary",
                    DiscordMarkerFailure::NotFound => "not found",
                };
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"kind": attachment.kind.marker_name(), "target": attachment.target, "reason": kind_label, "error": format!("{}", e)})), "dropping unresolved outbound attachment marker");
                failures.push(e.kind());
            }
        }
    }

    (local_files, remote_urls, failures)
}

/// Build the count-only delivery failure tail appended to the bot's reply
/// when at least one marker was dropped. Returns `None` when the failure
/// list is empty so callers can keep the body untouched.
pub(crate) fn delivery_failure_note(failures: &[DiscordMarkerFailure]) -> Option<String> {
    if failures.is_empty() {
        return None;
    }
    let count = failures.len().to_string();
    let key = if failures.len() == 1 {
        "channel-discord-delivery-failure-note-one"
    } else {
        "channel-discord-delivery-failure-note-many"
    };
    Some(i18n::get_required_cli_string_with_args(
        key,
        &[("count", count.as_str())],
    ))
}

/// Compose the final reply body with the delivery-failure note appended.
/// When the marker-stripped content is empty the note replaces the body;
/// otherwise the note follows the content separated by a blank line.
pub(crate) fn compose_body_with_failure_note(content: &str, note: Option<&str>) -> String {
    match note {
        Some(note) if content.trim().is_empty() => note.to_string(),
        Some(note) => format!("{content}\n\n{note}"),
        None => content.to_string(),
    }
}

/// Emoji reactions applied to the bot's own outgoing message based on which
/// kinds of marker failures occurred. 🚫 signals a trust-boundary refusal,
/// ⚠️ signals a post-validation delivery failure. Both can fire on the
/// same message when a batch mixes refusals and not-found targets.
pub(crate) fn decide_failure_reactions(failures: &[DiscordMarkerFailure]) -> Vec<&'static str> {
    let mut out = Vec::new();
    if failures
        .iter()
        .any(|k| matches!(k, DiscordMarkerFailure::Refused))
    {
        out.push("🚫");
    }
    if failures
        .iter()
        .any(|k| matches!(k, DiscordMarkerFailure::NotFound))
    {
        out.push("⚠️");
    }
    out
}

pub(crate) fn with_inline_attachment_urls(content: &str, remote_urls: &[String]) -> String {
    let mut lines = Vec::new();
    if !content.trim().is_empty() {
        lines.push(content.trim().to_string());
    }
    if !remote_urls.is_empty() {
        lines.extend(remote_urls.iter().cloned());
    }
    lines.join("\n")
}

#[cfg(test)]
mod embed_tests {
    use super::*;

    #[test]
    fn parses_a_single_embed_and_strips_it() {
        let (cleaned, specs) = parse_embed_markers(
            "before [EMBED:{\"title\":\"Hi\",\"description\":\"there\"}] after",
        );
        assert_eq!(cleaned, "before  after");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].title.as_deref(), Some("Hi"));
        assert_eq!(specs[0].description.as_deref(), Some("there"));
    }

    #[test]
    fn brace_aware_scan_tolerates_brackets_inside_json_strings() {
        // A naive first-`]` scan would truncate the JSON here.
        let (cleaned, specs) = parse_embed_markers("x [EMBED:{\"description\":\"a [b] c]\"}] y");
        assert_eq!(cleaned, "x  y");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].description.as_deref(), Some("a [b] c]"));
    }

    #[test]
    fn parses_nested_objects_and_fields_array() {
        let (_, specs) = parse_embed_markers(
            "[EMBED:{\"footer\":{\"text\":\"ft\"},\"fields\":[{\"name\":\"n\",\"value\":\"v\",\"inline\":true}]}]",
        );
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].footer.as_ref().unwrap().text, "ft");
        assert_eq!(specs[0].fields.len(), 1);
        assert!(specs[0].fields[0].inline);
    }

    #[test]
    fn malformed_marker_is_left_verbatim() {
        // Missing closing brace → not a valid marker, kept in the text.
        let (cleaned, specs) = parse_embed_markers("keep [EMBED:{not json] here");
        assert!(specs.is_empty());
        assert_eq!(cleaned, "keep [EMBED:{not json] here");
    }

    #[test]
    fn serde_rejected_span_is_kept_verbatim_and_not_rescanned_inside() {
        // The outer footer is missing its required `text`, so serde rejects the
        // whole (structurally complete) marker. The scanner must skip PAST the
        // span — not re-enter it and extract the nested `[EMBED:` sitting inside
        // the description string as a spurious embed.
        let msg = r#"x [EMBED:{"footer":{"icon_url":"u"},"description":"see [EMBED:{\"title\":\"INNER\"}] now"}] y"#;
        let (cleaned, specs) = parse_embed_markers(msg);
        assert!(
            specs.is_empty(),
            "no embed parsed: outer invalid, inner not re-scanned"
        );
        assert_eq!(
            cleaned,
            msg.trim(),
            "the whole rejected span is preserved verbatim"
        );
    }

    #[test]
    fn tag_is_case_insensitive() {
        let (cleaned, specs) = parse_embed_markers("[embed:{\"title\":\"T\"}]");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].title.as_deref(), Some("T"));
        assert_eq!(cleaned, "");
    }

    #[test]
    fn multiple_embeds_parse_in_order() {
        let (cleaned, specs) =
            parse_embed_markers("[EMBED:{\"title\":\"one\"}] mid [EMBED:{\"title\":\"two\"}]");
        assert_eq!(cleaned, "mid");
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].title.as_deref(), Some("one"));
        assert_eq!(specs[1].title.as_deref(), Some("two"));
    }

    #[test]
    fn spec_to_embed_keeps_http_image_and_links() {
        let spec = EmbedSpec {
            title: Some("T".to_string()),
            image: Some(EmbedMediaSpec::Url("https://example.com/i.png".to_string())),
            url: Some("http://example.com".to_string()),
            ..Default::default()
        };
        let (embed, failures) = spec_to_embed(spec, None);
        let embed = embed.expect("non-empty embed");
        assert_eq!(
            embed.image.as_ref().unwrap().url,
            "https://example.com/i.png"
        );
        assert_eq!(embed.url.as_deref(), Some("http://example.com"));
        assert!(failures.is_empty());
    }

    #[test]
    fn image_and_thumbnail_accept_discord_nested_object_and_bare_string() {
        // Discord models image/thumbnail as `{ "url": … }`, which is what an
        // agent following the "Discord embed JSON object" affordance emits.
        let (_, mut specs) = parse_embed_markers(
            r#"[EMBED:{"title":"T","image":{"url":"https://e.com/i.png"},"thumbnail":{"url":"https://e.com/t.png"}}]"#,
        );
        assert_eq!(
            specs.len(),
            1,
            "the nested-media embed parses (does not reject)"
        );
        let (embed, failures) = spec_to_embed(specs.remove(0), None);
        let embed = embed.expect("renders");
        assert_eq!(embed.image.as_ref().unwrap().url, "https://e.com/i.png");
        assert_eq!(embed.thumbnail.as_ref().unwrap().url, "https://e.com/t.png");
        assert!(failures.is_empty());

        // The bare-string form is still accepted.
        let (_, mut bare) = parse_embed_markers(r#"[EMBED:{"image":"https://e.com/b.png"}]"#);
        let (embed, _) = spec_to_embed(bare.remove(0), None);
        assert_eq!(embed.unwrap().image.unwrap().url, "https://e.com/b.png");
    }

    #[test]
    fn spec_to_embed_drops_disallowed_scheme_url_but_keeps_text() {
        let spec = EmbedSpec {
            title: Some("Kept".to_string()),
            image: Some(EmbedMediaSpec::Url("file:///etc/passwd".to_string())),
            ..Default::default()
        };
        let (embed, failures) = spec_to_embed(spec, None);
        let embed = embed.expect("text survives");
        assert_eq!(embed.title.as_deref(), Some("Kept"));
        assert!(embed.image.is_none());
        assert_eq!(failures, vec![DiscordMarkerFailure::Refused]);
    }

    #[test]
    fn spec_to_embed_drops_local_path_image_as_not_embeddable() {
        // A real, in-workspace file still cannot be referenced by URL in an
        // embed — Discord only fetches http(s). It must be refused, not Local.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pic.png");
        std::fs::write(&file, b"x").unwrap();
        let abs = std::fs::canonicalize(&file).unwrap();
        let spec = EmbedSpec {
            description: Some("body".to_string()),
            thumbnail: Some(EmbedMediaSpec::Url(abs.to_string_lossy().to_string())),
            ..Default::default()
        };
        let (embed, failures) = spec_to_embed(spec, Some(dir.path()));
        let embed = embed.expect("description survives");
        assert!(embed.thumbnail.is_none());
        assert_eq!(failures, vec![DiscordMarkerFailure::Refused]);
    }

    #[test]
    fn spec_to_embed_returns_none_when_nothing_renders() {
        let spec = EmbedSpec {
            image: Some(EmbedMediaSpec::Url("file:///etc/passwd".to_string())),
            ..Default::default()
        };
        let (embed, failures) = spec_to_embed(spec, None);
        assert!(embed.is_none());
        assert_eq!(failures, vec![DiscordMarkerFailure::Refused]);
    }
}

#[cfg(test)]
mod component_marker_tests {
    use super::*;
    use crate::discord::components::ButtonStyle;

    #[test]
    fn merges_multiple_markers_and_strips_all() {
        // Regression: an agent composing a "kitchen sink" reply may emit several
        // [COMPONENTS:…] markers (e.g. one per row). Previously only the first
        // was honored and the rest leaked as raw JSON "outside" the rendered
        // set. Now every marker is consumed, rows merged in order, all stripped.
        let msg = concat!(
            "Buttons: [COMPONENTS:{\"rows\":[[{\"label\":\"Primary\",\"style\":\"primary\",\"prompt\":\"p1\"}]]}]\n",
            "Modal: [COMPONENTS:{\"rows\":[[{\"label\":\"Report\",\"style\":\"danger\",\"prompt\":\"p2\",\"modal\":{\"title\":\"T\",\"fields\":[{\"id\":\"s\",\"label\":\"S\",\"style\":\"short\",\"required\":true}]}}]]}]\n",
            "Select: [COMPONENTS:{\"rows\":[[{\"select\":\"Pick\",\"options\":[{\"label\":\"A\",\"value\":\"a\",\"prompt\":\"pa\"}]}]]}]",
        );
        let (cleaned, rows) = parse_component_markers(msg);
        assert!(
            !cleaned.contains("[COMPONENTS:"),
            "no marker may leak; got: {cleaned:?}"
        );
        assert_eq!(rows.len(), 3, "rows from all three markers are merged");
        assert_eq!(rows[0].len(), 1);
        assert_eq!(rows[1].len(), 1);
        assert_eq!(rows[2].len(), 1);
        assert!(matches!(rows[2][0], ComponentSpec::Select { .. }));
    }

    #[test]
    fn repairs_dropped_rows_closing_bracket() {
        let msg = concat!(
            "[COMPONENTS:{\"rows\":[",
            "[{\"label\":\"A\",\"style\":\"primary\",\"prompt\":\"pa\"}],",
            "[{\"label\":\"B\",\"style\":\"danger\",\"prompt\":\"pb\"}]",
            "}]", // <- model dropped the rows-array ']' here
        );
        let (cleaned, rows) = parse_component_markers(msg);
        assert!(
            !cleaned.contains("[COMPONENTS:"),
            "marker must not leak raw; got {cleaned:?}"
        );
        assert_eq!(rows.len(), 2, "both rows recovered after bracket repair");
        assert_eq!(rows[0].len(), 1);
        assert_eq!(rows[1].len(), 1);
    }

    #[test]
    fn valid_kitchen_sink_payload_parses_to_three_rows() {
        // The maintainer-validated kitchen-sink body (buttons+link / modal /
        // select) must parse cleanly with no repair and strip fully.
        let msg = "[COMPONENTS:{\"rows\":[[{\"label\":\"Primary\",\"style\":\"primary\",\"prompt\":\"p\"},{\"label\":\"View PR\",\"url\":\"https://example.com\"}],[{\"label\":\"Report Bug\",\"style\":\"danger\",\"prompt\":\"p\",\"modal\":{\"title\":\"Report\",\"fields\":[{\"id\":\"summary\",\"label\":\"Summary\",\"style\":\"short\",\"required\":true}]}}],[{\"select\":\"Pick\",\"options\":[{\"label\":\"All OK\",\"value\":\"all_ok\",\"prompt\":\"o\"}]}]]}]";
        let (cleaned, rows) = parse_component_markers(msg);
        assert_eq!(cleaned, "");
        assert_eq!(rows.len(), 3);
        assert!(matches!(rows[1][0], ComponentSpec::ModalButton { .. }));
        assert!(matches!(rows[2][0], ComponentSpec::Select { .. }));
    }

    #[test]
    fn mid_message_under_closed_marker_preserves_prose() {
        let msg = "Status update: [COMPONENTS:{\"rows\":[[{\"label\":\"Ack\",\"prompt\":\"ack\"}]] all good, deploy finished] thanks everyone";
        let (cleaned, rows) = parse_component_markers(msg);
        assert!(
            cleaned.contains("all good, deploy finished"),
            "user prose must never be deleted; got {cleaned:?}"
        );
        assert!(
            cleaned.contains("thanks everyone"),
            "trailing prose must be preserved; got {cleaned:?}"
        );
        assert!(
            rows.is_empty(),
            "an ambiguous under-closed marker renders nothing"
        );
    }

    #[test]
    fn garbled_marker_does_not_poison_a_later_valid_marker() {
        // A first, unterminated `[COMPONENTS:` (no usable close) must neither
        // swallow nor suppress a later perfectly valid marker: the bad one is
        // left verbatim, the good one still renders.
        let msg = "[COMPONENTS:{\"rows\":[[ oops [COMPONENTS:{\"rows\":[[{\"label\":\"Go\",\"prompt\":\"go\"}]]}]";
        let (cleaned, rows) = parse_component_markers(msg);
        assert_eq!(rows.len(), 1, "the valid second marker renders");
        assert_eq!(rows[0].len(), 1);
        assert!(
            !cleaned.contains("\"Go\""),
            "the valid marker is stripped, not left in text; got {cleaned:?}"
        );
    }

    #[test]
    fn parses_button_row_and_strips_marker() {
        let msg = "Choose: [COMPONENTS:{\"rows\":[[{\"label\":\"Approve\",\"style\":\"success\",\"prompt\":\"approve it\"},{\"label\":\"Deny\",\"style\":\"danger\",\"prompt\":\"deny it\"}]]}] thanks";
        let (cleaned, rows) = parse_component_markers(msg);
        assert_eq!(cleaned, "Choose:  thanks");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 2);
        assert_eq!(
            rows[0][0],
            ComponentSpec::Button {
                label: "Approve".into(),
                style: ButtonStyle::Success,
                prompt: "approve it".into(),
            }
        );
        assert_eq!(
            rows[0][1],
            ComponentSpec::Button {
                label: "Deny".into(),
                style: ButtonStyle::Danger,
                prompt: "deny it".into(),
            }
        );
    }

    #[test]
    fn parses_link_button_without_prompt() {
        let (_, rows) = parse_component_markers(
            "[COMPONENTS:{\"rows\":[[{\"label\":\"Docs\",\"url\":\"https://example.com\"}]]}]",
        );
        assert_eq!(
            rows[0][0],
            ComponentSpec::Link {
                label: "Docs".into(),
                url: "https://example.com".into(),
            }
        );
    }

    #[test]
    fn parses_select_with_options() {
        let (_, rows) = parse_component_markers(
            "[COMPONENTS:{\"rows\":[[{\"select\":\"Pick one\",\"options\":[{\"label\":\"A\",\"value\":\"a\",\"prompt\":\"chose a\"},{\"label\":\"B\",\"value\":\"b\",\"prompt\":\"chose b\"}]}]]}]",
        );
        assert_eq!(rows.len(), 1);
        match &rows[0][0] {
            ComponentSpec::Select {
                placeholder,
                options,
            } => {
                assert_eq!(placeholder, "Pick one");
                assert_eq!(options.len(), 2);
                assert_eq!(options[0].label, "A");
                assert_eq!(options[0].value, "a");
                assert_eq!(options[0].prompt, "chose a");
                assert_eq!(options[1].prompt, "chose b");
            }
            other => panic!("expected select, got {other:?}"),
        }
    }

    #[test]
    fn unknown_style_defaults_to_secondary() {
        let (_, rows) = parse_component_markers(
            "[COMPONENTS:{\"rows\":[[{\"label\":\"X\",\"prompt\":\"p\"}]]}]",
        );
        assert!(matches!(
            rows[0][0],
            ComponentSpec::Button {
                style: ButtonStyle::Secondary,
                ..
            }
        ));
    }

    #[test]
    fn nested_brackets_in_prompt_dont_truncate_marker() {
        // A prompt containing `]` (and a JSON array) must not end the marker early.
        let msg =
            "[COMPONENTS:{\"rows\":[[{\"label\":\"Go\",\"prompt\":\"run [tool] now ]]\"}]]}] tail";
        let (cleaned, rows) = parse_component_markers(msg);
        assert_eq!(cleaned, "tail");
        assert_eq!(rows.len(), 1);
        match &rows[0][0] {
            ComponentSpec::Button { prompt, .. } => assert_eq!(prompt, "run [tool] now ]]"),
            other => panic!("expected button, got {other:?}"),
        }
    }

    #[test]
    fn unparseable_body_left_verbatim_no_prose_lost() {
        // A balanced body that doesn't parse into rows is NOT a renderable marker.
        // We leave the raw tag verbatim (a recoverable leak) rather than strip a
        // slice we can't validate — surrounding prose is always preserved.
        let (cleaned, rows) = parse_component_markers("before [COMPONENTS:{not valid json}] after");
        assert!(cleaned.contains("before"), "got {cleaned:?}");
        assert!(cleaned.contains("after"), "got {cleaned:?}");
        assert!(rows.is_empty());
    }

    #[test]
    fn over_opened_body_does_not_eat_prose() {
        // The model doubled the opening brace, so strict_marker_close walks past
        // the marker's own `]` (still at depth 1) onto a `]` in trailing prose.
        // The over-opened body fails to parse, so Case 1 does NOT strip through it:
        // all prose is preserved and nothing renders. (Data-loss regression guard.)
        let msg = "Heads up: [COMPONENTS:{{\"rows\":[[{\"label\":\"A\",\"prompt\":\"p\"}]]}] trailing ] here and more";
        let (cleaned, rows) = parse_component_markers(msg);
        assert!(
            cleaned.contains("trailing"),
            "prose before the false close kept; got {cleaned:?}"
        );
        assert!(
            cleaned.contains("here and more"),
            "prose after kept; got {cleaned:?}"
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn dropped_outer_close_with_prose_does_not_eat_prose() {
        // The `{…}` body is itself balanced but the marker's own closing `]` is
        // dropped and a later `]` sits in prose. The contaminated slice (body +
        // prose) fails to parse, so no words are deleted. (Data-loss regression.)
        let msg = "Update [COMPONENTS:{\"rows\":[[{\"label\":\"A\",\"prompt\":\"p\"}]]} the value is arr] later";
        let (cleaned, rows) = parse_component_markers(msg);
        assert!(cleaned.contains("the value is arr"), "got {cleaned:?}");
        assert!(cleaned.contains("later"), "got {cleaned:?}");
        assert!(rows.is_empty());
    }

    #[test]
    fn literal_tag_in_prose_does_not_eat_prose() {
        // A user who literally types the substring `[COMPONENTS:` in normal prose
        // with a later `]` must not have the text between them deleted.
        let msg = "see the [COMPONENTS: docs] for the format and more notes here";
        let (cleaned, rows) = parse_component_markers(msg);
        assert!(cleaned.contains("docs"), "got {cleaned:?}");
        assert!(cleaned.contains("more notes here"), "got {cleaned:?}");
        assert!(rows.is_empty());
    }

    #[test]
    fn missing_close_bracket_leaves_text_untouched() {
        // No balanced close — it isn't a marker, so the text is returned as-is.
        let msg = "[COMPONENTS:{\"rows\":[[{\"label\":\"x\"}]]} no close";
        let (cleaned, rows) = parse_component_markers(msg);
        assert_eq!(cleaned, msg);
        assert!(rows.is_empty());
    }

    #[test]
    fn no_marker_returns_input_unchanged() {
        let (cleaned, rows) = parse_component_markers("just a normal message");
        assert_eq!(cleaned, "just a normal message");
        assert!(rows.is_empty());
    }

    #[test]
    fn parses_modal_button_with_fields() {
        use crate::discord::components::TextInputStyle;
        let (cleaned, rows) = parse_component_markers(
            "Open: [COMPONENTS:{\"rows\":[[{\"label\":\"Report\",\"style\":\"danger\",\"prompt\":\"file it\",\"modal\":{\"title\":\"Report\",\"fields\":[{\"id\":\"reason\",\"label\":\"Reason\",\"style\":\"paragraph\",\"required\":true,\"placeholder\":\"why?\",\"min\":1,\"max\":500}]}}]]}]",
        );
        assert_eq!(cleaned, "Open:");
        match &rows[0][0] {
            ComponentSpec::ModalButton {
                label,
                style,
                prompt,
                modal,
            } => {
                assert_eq!(label, "Report");
                assert_eq!(*style, ButtonStyle::Danger);
                assert_eq!(prompt, "file it");
                assert_eq!(modal.title, "Report");
                assert_eq!(modal.fields.len(), 1);
                let f = &modal.fields[0];
                assert_eq!(f.id, "reason");
                assert_eq!(f.label, "Reason");
                assert_eq!(f.style, TextInputStyle::Paragraph);
                assert!(f.required);
                assert_eq!(f.placeholder.as_deref(), Some("why?"));
                assert_eq!(f.min_length, Some(1));
                assert_eq!(f.max_length, Some(500));
            }
            other => panic!("expected modal button, got {other:?}"),
        }
    }

    #[test]
    fn modal_button_without_fields_is_dropped() {
        // A modal with no renderable fields drops the whole button (it can't open
        // an empty form), rather than rendering or 400-ing the send.
        let (cleaned, rows) = parse_component_markers(
            "hi [COMPONENTS:{\"rows\":[[{\"label\":\"X\",\"modal\":{\"title\":\"t\",\"fields\":[]}}]]}]",
        );
        assert_eq!(cleaned, "hi");
        assert!(rows.is_empty());
    }

    #[test]
    fn empty_options_select_is_dropped() {
        // A select with no renderable options yields no rows (dropped, not 400).
        let (cleaned, rows) = parse_component_markers(
            "hi [COMPONENTS:{\"rows\":[[{\"select\":\"p\",\"options\":[]}]]}]",
        );
        assert_eq!(cleaned, "hi");
        assert!(rows.is_empty());
    }
}
