//! Contract layer for the Discord channel: the data shapes that cross module
//! boundaries. Zero business logic beyond trivial serialisation/parsing of the
//! types themselves — implementation modules (`rest`, `interaction`, `slash`,
//! `markers`, `chunk`) and the `mod.rs` wiring depend on these; nothing here
//! depends on them.

use std::sync::Arc;

use super::embed::DiscordEmbed;
use super::slash_options::OptionSpec;

// ─────────────────────────────────────────────────────────────────────────────
// Outbound message envelope
//
// The single payload the channel-message REST builders collapse onto. The
// builders already route through `text()`/`to_rest_json()` (EPIC A Phase 2), so
// the struct and its methods are live; `to_rest_json` is byte-identical to the
// historical `json!({ "content": content })` when only `content` is populated
// (proven by the tests below). EPIC C now fills `embeds` (serialized here);
// EPIC B fills `components`/`flags` — until then those two stay unread, so the
// `#[allow(dead_code)]` is scoped to just them.
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub(crate) struct DiscordOutgoing {
    pub(crate) content: Option<String>,
    pub(crate) embeds: Vec<DiscordEmbed>,
    // Unread until EPIC B wires these into `to_rest_json`; the allow is on the
    // placeholder fields only, leaving the struct itself under dead-code analysis.
    #[allow(dead_code)]
    pub(crate) components: Vec<DiscordActionRow>,
    #[allow(dead_code)]
    pub(crate) flags: DiscordMessageFlags,
}

/// Placeholder — filled by EPIC B (components).
#[allow(dead_code)]
#[derive(Debug, Default, Clone)]
pub(crate) struct DiscordActionRow;

/// Message flags (e.g. ephemeral); zero by default and omitted from the payload
/// when zero. Filled by EPIC B.
#[allow(dead_code)]
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct DiscordMessageFlags(pub(crate) u64);

impl DiscordOutgoing {
    /// A content-only payload (no embeds/components/flags) — the shape every
    /// channel-message builder produces today. EPIC C/B add embed/component
    /// constructors alongside this one.
    pub(crate) fn text(content: impl Into<String>) -> Self {
        Self {
            content: Some(content.into()),
            ..Default::default()
        }
    }

    /// Build the REST message JSON. Keys for `embeds`/`components`/`flags` are
    /// omitted while empty/zero, so a content-only payload serialises to exactly
    /// `{"content": <content>}` — the behaviour-neutrality invariant for EPIC A.
    pub(crate) fn to_rest_json(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        if let Some(content) = &self.content {
            obj.insert(
                "content".to_string(),
                serde_json::Value::String(content.clone()),
            );
        }
        if !self.embeds.is_empty() {
            let embeds: Vec<serde_json::Value> =
                self.embeds.iter().map(DiscordEmbed::to_api).collect();
            obj.insert("embeds".to_string(), serde_json::Value::Array(embeds));
        }
        // EPIC B adds `components`/`flags` here; empty → omitted.
        serde_json::Value::Object(obj)
    }

    /// The same payload as a string, for the `payload_json` multipart field.
    pub(crate) fn payload_json(&self) -> String {
        self.to_rest_json().to_string()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Slash-command specs (produced by `slash`, consumed by the orchestrator)
// ─────────────────────────────────────────────────────────────────────────────

/// A slash command derived from an installed skill. `slug` is the Discord
/// command name; `skill_name` is the skill's manifest name (sanitized of
/// quotes and newlines at spec-build time, since it is interpolated into
/// the synthesized agent prompt); `description` is truncated to Discord's
/// 100-char limit; `options` are the typed command options (empty → the legacy
/// single free-text `input`).
///
/// `Eq` is not derived: [`OptionSpec`] carries `f64` numeric bounds.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscordSlashCommandSpec {
    pub skill_name: String,
    pub slug: String,
    pub description: String,
    pub options: Vec<OptionSpec>,
}

/// Resolves the current skill-derived command set from canonical state at
/// READY/interaction time. No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE
/// SOURCE OF TRUTH") — skills install/uninstall at runtime. The loader does
/// blocking file IO, so callers must run it via `spawn_blocking`, never on
/// the gateway listen loop.
pub type DiscordSlashCommandResolver = Arc<dyn Fn() -> Vec<DiscordSlashCommandSpec> + Send + Sync>;

/// Outcome of a slash-command reconcile pass.
#[derive(Debug)]
pub(crate) enum ReconcileOutcome {
    /// The command set was reconciled (or was already current).
    Reconciled,
    /// Discord rate-limited the pass; the caller must persist this cooldown and
    /// not retry until the given unix-seconds deadline.
    RateLimited { until: i64 },
}

// ─────────────────────────────────────────────────────────────────────────────
// Listener fatal error (constructed in the gateway loop, downcast by the
// orchestrator's component supervisor)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub(crate) struct DiscordListenerFatalError {
    message: String,
}

impl DiscordListenerFatalError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DiscordListenerFatalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DiscordListenerFatalError {}

// ─────────────────────────────────────────────────────────────────────────────
// Interaction reply-target sentinel codec (the wire vocabulary the channel and
// the interaction module both speak)
// ─────────────────────────────────────────────────────────────────────────────

/// Reply-target sentinel prefix marking a ChannelMessage that must be answered
/// via the interaction followup webhook rather than a normal channel message.
pub(crate) const DISCORD_INTERACTION_PREFIX: &str = "interaction:";

/// Build the sentinel reply target carrying only the interaction id. The
/// bearer token deliberately never enters the reply target: reply targets
/// flow into logs, session keys (and thus on-disk filenames), and memory
/// rows — `send()` resolves the credentials from the channel-local
/// `pending_interactions` store instead.
pub(crate) fn discord_interaction_reply_target(interaction_id: &str) -> String {
    format!("{DISCORD_INTERACTION_PREFIX}{interaction_id}")
}

/// Parse `interaction:{interaction_id}` back into the id. Rejects empty ids
/// and anything with extra segments (the legacy `app:token` form must never
/// round-trip as valid).
pub(crate) fn parse_discord_interaction_target(target: &str) -> Option<&str> {
    let id = target.strip_prefix(DISCORD_INTERACTION_PREFIX)?;
    if id.is_empty() || id.contains(':') {
        return None;
    }
    Some(id)
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared constants
// ─────────────────────────────────────────────────────────────────────────────

/// Discord's maximum message length for regular messages.
///
/// Discord rejects longer payloads with `50035 Invalid Form Body`.
pub(crate) const DISCORD_MAX_MESSAGE_LENGTH: usize = 2000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_only_payload_is_byte_identical_to_legacy() {
        let out = DiscordOutgoing {
            content: Some("hello".to_string()),
            ..Default::default()
        };
        // The historical builders emitted `json!({ "content": content })`.
        assert_eq!(
            out.to_rest_json(),
            serde_json::json!({ "content": "hello" })
        );
        assert_eq!(
            out.payload_json(),
            serde_json::json!({ "content": "hello" }).to_string()
        );
    }

    #[test]
    fn empty_content_still_emits_the_key() {
        let out = DiscordOutgoing {
            content: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(out.to_rest_json(), serde_json::json!({ "content": "" }));
    }

    #[test]
    fn populated_embeds_serialize_through_the_chokepoint() {
        let out = DiscordOutgoing {
            content: Some("see below".to_string()),
            embeds: vec![DiscordEmbed {
                title: Some("Report".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            out.to_rest_json(),
            serde_json::json!({
                "content": "see below",
                "embeds": [{ "title": "Report" }]
            })
        );
    }

    #[test]
    fn empty_embeds_vec_omits_the_key_preserving_byte_identity() {
        // An explicitly-empty embeds vec must not grow an `"embeds"` key, or the
        // EPIC A content-only byte-identity invariant breaks.
        let out = DiscordOutgoing {
            content: Some("hi".to_string()),
            embeds: Vec::new(),
            ..Default::default()
        };
        assert_eq!(out.to_rest_json(), serde_json::json!({ "content": "hi" }));
    }

    #[test]
    fn absent_content_omits_the_key() {
        assert_eq!(
            DiscordOutgoing::default().to_rest_json(),
            serde_json::json!({})
        );
    }

    #[test]
    fn interaction_sentinel_round_trips_and_rejects_token_form() {
        let target = discord_interaction_reply_target("abc123");
        assert_eq!(target, "interaction:abc123");
        assert_eq!(parse_discord_interaction_target(&target), Some("abc123"));
        // legacy app:token form and empty id must not round-trip
        assert_eq!(
            parse_discord_interaction_target("interaction:app:tok"),
            None
        );
        assert_eq!(parse_discord_interaction_target("interaction:"), None);
        assert_eq!(parse_discord_interaction_target("nope"), None);
    }
}
