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
}

/// The account referenced by a `mention` entity.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mentioned {
    pub id: String,
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

/// Strip `<at>…</at>` mention tags (tag and inner name) from message
/// text, collapsing the surrounding whitespace. Teams inserts these for
/// every @mention, including the bot's own.
#[must_use]
pub fn strip_mention_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<at>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</at>") {
            Some(end_rel) => rest = &rest[start + end_rel + "</at>".len()..],
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

/// Full inbound text cleanup: strip mention tags, then decode entities.
#[must_use]
pub fn clean_message_text(text: &str) -> String {
    decode_html_entities(&strip_mention_tags(text))
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
    fn mention_tags_are_stripped() {
        assert_eq!(
            strip_mention_tags("<at>ZeroClaw</at> run the report"),
            "run the report"
        );
        assert_eq!(
            strip_mention_tags("hey <at>ZeroClaw</at>, and <at>Alice</at> too"),
            "hey , and too"
        );
        assert_eq!(strip_mention_tags("no mentions here"), "no mentions here");
        assert_eq!(
            strip_mention_tags("broken <at>tag stays"),
            "broken <at>tag stays"
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
            clean_message_text("<at>ZeroClaw</at> 1 &lt; 2 &amp;&amp; 3 &gt; 2"),
            "1 < 2 && 3 > 2"
        );
    }
}
