//! Bot Framework activity types and inbound text handling.
//!
//! Serde views over the activity JSON Teams POSTs to the listener, plus
//! the text-cleanup helpers (mention-tag stripping, HTML entity decoding)
//! and conversation-id normalization documented in
//! `docs/msteams-channel-design.md` §3.

use serde::Deserialize;

/// Inbound Bot Framework activity (the fields the channel consumes).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    /// `message`, `typing`, `conversationUpdate`, ... Only `message`
    /// produces a `ChannelMessage`.
    #[serde(rename = "type")]
    pub activity_type: String,
    /// Platform activity id (used as the reply/thread anchor).
    #[serde(default)]
    pub id: Option<String>,
    /// Originating channel id (`msteams` for Teams). Bound to the signing
    /// key's endorsements during authentication: the listener rejects an
    /// activity whose `channelId` the signing key is not published to sign
    /// for. Absent on some activities from older Connector versions.
    #[serde(default)]
    pub channel_id: Option<String>,
    /// RFC 3339 timestamp.
    #[serde(default)]
    pub timestamp: Option<String>,
    /// Bot Connector base URL for this conversation. Required for
    /// outbound replies; delivered on every activity.
    #[serde(default)]
    pub service_url: Option<String>,
    #[serde(default)]
    pub from: Option<ChannelAccount>,
    #[serde(default)]
    pub recipient: Option<ChannelAccount>,
    #[serde(default)]
    pub conversation: Option<ConversationAccount>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub entities: Vec<Entity>,
}

/// A user or bot identity on an activity.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAccount {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    /// Entra object id of the user, when Teams provides it. Stable
    /// across conversations (unlike the `29:` channel-scoped id), so
    /// peer-group allowlists match on it.
    #[serde(default)]
    pub aad_object_id: Option<String>,
}

/// The conversation an activity belongs to.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationAccount {
    pub id: String,
    /// `personal`, `groupChat`, or `channel` (absent on some activities).
    #[serde(default)]
    pub conversation_type: Option<String>,
}

/// Activity entity; the channel only interprets `mention` entries.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entity {
    #[serde(rename = "type")]
    pub entity_type: String,
    #[serde(default)]
    pub mentioned: Option<Mentioned>,
    /// Literal `<at>…</at>` substring this mention occupies in the message
    /// text. Teams includes it on `mention` entities so the exact span can
    /// be located; used to strip only the bot's own mention while keeping
    /// other mentioned users' names in the prompt.
    #[serde(default)]
    pub text: Option<String>,
}

impl Entity {
    /// The `<at>…</at>` literal this mention occupies, preferring the
    /// entity's own `text` and falling back to reconstructing it from the
    /// mentioned display name.
    #[must_use]
    pub fn mention_literal(&self) -> Option<String> {
        if let Some(text) = self.text.as_deref().filter(|t| !t.is_empty()) {
            return Some(text.to_string());
        }
        self.mentioned
            .as_ref()
            .and_then(|mentioned| mentioned.name.as_deref())
            .map(|name| format!("<at>{name}</at>"))
    }
}

/// The account referenced by a `mention` entity.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mentioned {
    pub id: String,
    /// Display name Teams rendered inside the `<at>…</at>` tag. Used as a
    /// fallback to locate the mention span when the entity omits `text`.
    #[serde(default)]
    pub name: Option<String>,
}

impl Activity {
    /// Whether this activity lives in a personal (1:1) conversation.
    #[must_use]
    pub fn is_personal(&self) -> bool {
        self.conversation
            .as_ref()
            .and_then(|c| c.conversation_type.as_deref())
            == Some("personal")
    }

    /// Whether any `mention` entity targets `bot_id`.
    #[must_use]
    pub fn mentions(&self, bot_id: &str) -> bool {
        self.entities.iter().any(|entity| {
            entity.entity_type == "mention"
                && entity
                    .mentioned
                    .as_ref()
                    .is_some_and(|mentioned| mentioned.id == bot_id)
        })
    }

    /// The `<at>…</at>` literals for the bot's own mentions (`bot_id`).
    /// These are stripped entirely from the prompt; every other mention is
    /// unwrapped to its display name so it survives into model ingress.
    #[must_use]
    pub fn bot_mention_literals(&self, bot_id: &str) -> Vec<String> {
        self.entities
            .iter()
            .filter(|entity| entity.entity_type == "mention")
            .filter(|entity| {
                entity
                    .mentioned
                    .as_ref()
                    .is_some_and(|mentioned| mentioned.id == bot_id)
            })
            .filter_map(Entity::mention_literal)
            .collect()
    }

    /// Activity timestamp as Unix seconds; `0` when absent or unparsable
    /// (matching other channels' fallback for missing timestamps).
    #[must_use]
    pub fn timestamp_secs(&self) -> u64 {
        self.timestamp
            .as_deref()
            .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
            .and_then(|dt| u64::try_from(dt.timestamp()).ok())
            .unwrap_or(0)
    }
}

/// Split a Teams conversation id into its base id and the optional
/// `;messageid=` thread suffix. Team-channel ids arrive as
/// `19:...@thread.tacv2;messageid=1234` when the message is inside a
/// thread; replies address the base id and thread on the message id.
#[must_use]
pub fn split_conversation_id(raw: &str) -> (&str, Option<&str>) {
    match raw.split_once(";messageid=") {
        Some((base, message_id)) if !message_id.is_empty() => (base, Some(message_id)),
        Some((base, _)) => (base, None),
        None => (raw, None),
    }
}

/// Unwrap `<at>…</at>` mention tags to their inner display name,
/// collapsing surrounding whitespace. Teams inserts these tags around
/// every @mention; unwrapping (rather than deleting) keeps the mentioned
/// user's name in the text, so a prompt like `@Bot ask @Alice` still
/// carries "Alice" to the model. The bot's own mention is removed
/// upstream by [`clean_message_text`] before this runs.
#[must_use]
pub fn unwrap_mention_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<at>") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "<at>".len()..];
        match after.find("</at>") {
            Some(end_rel) => {
                // Keep the inner display name, drop only the tags.
                out.push_str(&after[..end_rel]);
                rest = &after[end_rel + "</at>".len()..];
            }
            None => {
                // Unclosed tag: keep the remainder verbatim rather than
                // dropping user text.
                out.push_str(&rest[start..]);
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decode the HTML entities Teams substitutes into plain-text message
/// bodies. Deliberately the minimal named set plus numeric forms — this
/// is not a general HTML parser.
#[must_use]
pub fn decode_html_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let Some(end) = tail.find(';').filter(|&end| end <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix('#')
                .and_then(|digits| {
                    digits.strip_prefix('x').map_or_else(
                        || digits.parse::<u32>().ok(),
                        |hex| u32::from_str_radix(hex, 16).ok(),
                    )
                })
                .and_then(char::from_u32),
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Full inbound text cleanup: remove the bot's own `<at>…</at>` mentions
/// entirely, unwrap every remaining mention tag to its display name, then
/// decode HTML entities. `bot_mention_literals` come from
/// [`Activity::bot_mention_literals`].
#[must_use]
pub fn clean_message_text(text: &str, bot_mention_literals: &[String]) -> String {
    let mut without_bot = text.to_string();
    for literal in bot_mention_literals {
        if !literal.is_empty() {
            // Replace with a space so adjacent words don't fuse; the
            // whitespace collapse in `unwrap_mention_tags` tidies up.
            without_bot = without_bot.replace(literal.as_str(), " ");
        }
    }
    decode_html_entities(&unwrap_mention_tags(&without_bot))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: serde_json::Value) -> Activity {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn personal_message_activity_deserializes() {
        let activity = parse(serde_json::json!({
            "type": "message",
            "id": "1712345",
            "timestamp": "2026-07-18T02:00:00.000Z",
            "serviceUrl": "https://smba.trafficmanager.net/teams/",
            "channelId": "msteams",
            "from": { "id": "29:user-x", "name": "User X", "aadObjectId": "00000000-0000-0000-0000-00000000feed" },
            "recipient": { "id": "28:bot-app-id", "name": "ZeroClaw" },
            "conversation": { "id": "a:1conv", "conversationType": "personal" },
            "text": "hello"
        }));
        assert_eq!(activity.activity_type, "message");
        assert_eq!(activity.channel_id.as_deref(), Some("msteams"));
        assert!(activity.is_personal());
        assert_eq!(activity.from.as_ref().unwrap().id, "29:user-x");
        assert_eq!(
            activity.from.as_ref().unwrap().aad_object_id.as_deref(),
            Some("00000000-0000-0000-0000-00000000feed")
        );
        assert_eq!(
            activity.service_url.as_deref(),
            Some("https://smba.trafficmanager.net/teams/")
        );
        assert!(activity.timestamp_secs() > 1_700_000_000);
    }

    #[test]
    fn channel_activity_with_mention_entities() {
        let activity = parse(serde_json::json!({
            "type": "message",
            "conversation": {
                "id": "19:general@thread.tacv2;messageid=1700000000000",
                "conversationType": "channel"
            },
            "text": "<at>ZeroClaw</at> status?",
            "entities": [
                { "type": "clientInfo", "locale": "en-US" },
                { "type": "mention", "mentioned": { "id": "28:bot-app-id", "name": "ZeroClaw" }, "text": "<at>ZeroClaw</at>" }
            ]
        }));
        assert!(!activity.is_personal());
        assert!(activity.mentions("28:bot-app-id"));
        assert!(!activity.mentions("28:someone-else"));
    }

    #[test]
    fn minimal_conversation_update_deserializes() {
        let activity = parse(serde_json::json!({ "type": "conversationUpdate" }));
        assert_eq!(activity.activity_type, "conversationUpdate");
        assert!(!activity.is_personal());
        assert!(!activity.mentions("28:bot"));
        assert_eq!(activity.timestamp_secs(), 0);
    }

    #[test]
    fn conversation_id_thread_suffix_is_split() {
        assert_eq!(
            split_conversation_id("19:general@thread.tacv2;messageid=1700"),
            ("19:general@thread.tacv2", Some("1700"))
        );
        assert_eq!(
            split_conversation_id("19:general@thread.tacv2"),
            ("19:general@thread.tacv2", None)
        );
        assert_eq!(split_conversation_id("a:1conv"), ("a:1conv", None));
        assert_eq!(
            split_conversation_id("19:x@thread.tacv2;messageid="),
            ("19:x@thread.tacv2", None)
        );
    }

    #[test]
    fn mention_tags_are_unwrapped_to_names() {
        // Unwrapping keeps every mentioned name; the bot's own mention is
        // removed separately by clean_message_text, not here.
        assert_eq!(
            unwrap_mention_tags("<at>ZeroClaw</at> run the report"),
            "ZeroClaw run the report"
        );
        assert_eq!(
            unwrap_mention_tags("hey <at>ZeroClaw</at>, and <at>Alice</at> too"),
            "hey ZeroClaw, and Alice too"
        );
        assert_eq!(unwrap_mention_tags("no mentions here"), "no mentions here");
        assert_eq!(
            unwrap_mention_tags("broken <at>tag stays"),
            "broken <at>tag stays"
        );
    }

    #[test]
    fn bot_mention_literals_target_only_the_bot() {
        let activity = parse(serde_json::json!({
            "type": "message",
            "recipient": { "id": "28:bot-app-id", "name": "ZeroClaw" },
            "text": "<at>ZeroClaw</at> ask <at>Alice</at>",
            "entities": [
                { "type": "mention", "mentioned": { "id": "28:bot-app-id", "name": "ZeroClaw" }, "text": "<at>ZeroClaw</at>" },
                { "type": "mention", "mentioned": { "id": "29:alice", "name": "Alice" }, "text": "<at>Alice</at>" }
            ]
        }));
        assert_eq!(
            activity.bot_mention_literals("28:bot-app-id"),
            vec!["<at>ZeroClaw</at>".to_string()]
        );
    }

    #[test]
    fn bot_mention_literal_falls_back_to_display_name() {
        // Older Connector payloads omit the entity `text`; reconstruct the
        // literal from the mentioned display name instead.
        let entity: Entity = serde_json::from_value(serde_json::json!({
            "type": "mention",
            "mentioned": { "id": "28:bot", "name": "ZeroClaw" }
        }))
        .unwrap();
        assert_eq!(
            entity.mention_literal().as_deref(),
            Some("<at>ZeroClaw</at>")
        );
    }

    #[test]
    fn clean_message_text_drops_bot_mention_keeps_others() {
        let bot = ["<at>ZeroClaw</at>".to_string()];
        // Bot mention removed; the other mentioned user's name survives.
        assert_eq!(
            clean_message_text("<at>ZeroClaw</at> ask <at>Alice</at> to review", &bot),
            "ask Alice to review"
        );
        // No bot literals: every mention is unwrapped to its name.
        assert_eq!(
            clean_message_text("<at>Alice</at> and <at>Bob</at>", &[]),
            "Alice and Bob"
        );
    }

    #[test]
    fn html_entities_are_decoded() {
        assert_eq!(
            decode_html_entities("a &amp; b &lt;c&gt; &quot;d&quot; &#39;e&#39;&nbsp;f"),
            "a & b <c> \"d\" 'e' f"
        );
        assert_eq!(decode_html_entities("&#128075; &#x1F44B;"), "👋 👋");
        assert_eq!(
            decode_html_entities("unknown &entity; stays"),
            "unknown &entity; stays"
        );
        assert_eq!(decode_html_entities("bare & ampersand"), "bare & ampersand");
    }

    #[test]
    fn clean_message_text_combines_both() {
        assert_eq!(
            clean_message_text(
                "<at>ZeroClaw</at> 1 &lt; 2 &amp;&amp; 3 &gt; 2",
                &["<at>ZeroClaw</at>".to_string()]
            ),
            "1 < 2 && 3 > 2"
        );
    }
}
