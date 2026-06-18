//! Outbound media markers and the egress trust boundary.
//!
//! The agent emits `[IMAGE:…]` / `[DOCUMENT:…]` / `[VIDEO:…]` / `[AUDIO:…]` /
//! `[VOICE:…]` markers in its reply text. This module parses them out, validates
//! each target against the workspace sandbox (only `http(s)` URLs and absolute
//! paths inside `workspace_dir` may be exposed to chatters), and renders the
//! count-only delivery-failure note and the 🚫/⚠️ reactions when a target is
//! dropped.

use anyhow::Context as _;
use std::path::{Path, PathBuf};
use zeroclaw_runtime::i18n;

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

/// Resolved outbound attachment target after sandbox validation.
#[derive(Debug)]
pub(crate) enum DiscordMarkerTarget {
    Local(PathBuf),
    Http(String),
}

/// Why a marker target was rejected. Drives the user-facing emoji reaction
/// on the bot's outgoing message: `Refused` (trust-boundary rejection) maps
/// to 🚫, `NotFound` (path didn't resolve on disk) maps to ⚠️. The
/// distinction matters because a chatter should see at a glance that the
/// bot deliberately declined a target rather than tried and failed.
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

/// Validate an outbound marker target against Discord's trust-boundary policy.
///
/// The orchestrator system prompt mandates absolute paths for media markers,
/// and the workspace is the only directory the agent is authorised to
/// expose to chatters:
///
/// * `http`/`https` URLs are accepted and inlined as links.
/// * Any other URL scheme (`file:`, `data:`, custom `://`) is refused.
/// * Local paths must be absolute. Relative paths are agent
///   misconfiguration and dropped, not silently resolved against cwd.
/// * Absolute paths are canonicalised and must resolve inside
///   `workspace_dir`. Anything outside or any traversal escape is
///   refused; a path that simply doesn't exist on disk returns
///   `NotFound`, which the caller renders differently from a refusal.
/// * When `workspace_dir` is not configured, no local path can be safely
///   bounded, so all local targets are refused.
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
