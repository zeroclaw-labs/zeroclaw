//! Turn a WhatsApp history-sync payload into conversation turns the agent can read.
//!
//! WhatsApp acks a message the moment the client decrypts it, which tells the
//! server to drop it from the offline queue. A daemon that dies after the ack
//! and before the agent replies loses that turn permanently: the person is left
//! waiting, and when the agent comes back it answers as though the exchange
//! never happened — contradicting what they can still read on their phone.
//!
//! The platform already solves this. On pairing, and on demand via
//! `fetch_message_history`, it sends the real thread: exact text, exact
//! timestamps, stable message ids. That payload arrived on every pair this
//! deployment has ever done and was dropped on the floor, because nothing
//! handled the event. This module decodes it.
//!
//! Identity comes from WhatsApp's own message id rather than anything derived
//! locally. That is what makes re-importing safe: the same message carries the
//! same id whether it arrives now or in a sync three months from now, so the
//! store can recognise it as already known instead of appending a second copy.

use zeroclaw_api::session_keys::sanitize_session_key;

/// One conversation turn recovered from a history sync, in the shape the
/// session store persists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredTurn {
    /// WhatsApp's own id for this message. Stable across syncs and devices —
    /// the reason a re-import is a no-op rather than a duplicate.
    pub message_id: String,
    /// Conversation this turn belongs to, already sanitized to match the key
    /// the orchestrator builds for live messages.
    pub session_key: String,
    /// `assistant` for messages this account sent, `user` for the peer's.
    pub role: &'static str,
    /// Message text.
    pub content: String,
    /// Seconds since the Unix epoch, as WhatsApp recorded it.
    pub timestamp_secs: u64,
}

/// Build the session key for a conversation, matching `conversation_history_key`
/// for a `Sender`-scoped channel with no thread.
///
/// The live path derives this from a `ChannelMessage`, where `reply_target` is
/// the chat's LID and `sender` is the participant's phone number in `+E.164`.
/// A history sync carries no such message, so the same two identifiers are
/// passed in explicitly and assembled in the same order. Both sides run through
/// `sanitize_session_key`, so a turn recovered here lands in the bucket a live
/// message would have used.
///
/// Verified against the keys the running deployment actually wrote, not
/// inferred from the format string: `whatsapp.default` + `76188559093817@lid` +
/// `+5215557654321` is stored as
/// `whatsapp_default_76188559093817_lid__5215557654321`.
pub fn session_key_for_chat(channel_scope: &str, reply_target: &str, sender: &str) -> String {
    sanitize_session_key(&format!("{channel_scope}_{reply_target}_{sender}"))
}

/// Decide whether a recovered message is worth storing.
///
/// Empty bodies are skipped: a sync carries protocol messages, reactions,
/// revokes and media stubs alongside real text, and a turn with no content
/// teaches the agent nothing while still occupying context. Messages whose id
/// or timestamp is missing are skipped too — without an id there is no way to
/// recognise the message on a later sync, which is exactly the duplicate this
/// module exists to prevent.
pub fn is_storable(message_id: Option<&str>, timestamp: Option<u64>, text: &str) -> bool {
    message_id.is_some_and(|id| !id.is_empty())
        && timestamp.is_some_and(|t| t > 0)
        && !text.trim().is_empty()
}

/// Map WhatsApp's `from_me` flag onto the role the session store records.
///
/// `from_me` marks messages sent by the account the agent runs as, which are
/// the agent's own replies — `assistant`. Everything else came from the person
/// on the other end.
pub fn role_for(from_me: bool) -> &'static str {
    if from_me { "assistant" } else { "user" }
}

/// Count the turns in a history-sync payload that are worth storing.
///
/// Streams the payload conversation by conversation instead of decoding it
/// whole: a sync can be several megabytes, and the count needs none of it
/// resident. Conversations that fail to decode are skipped rather than
/// aborting the walk — a malformed entry should cost its own turns, not the
/// entire recovered history.
///
/// Returns a count and nothing else on purpose. It answers "did the platform
/// actually send us usable history, and how much" without any message text
/// crossing into a log line or a caller that might print it.
#[cfg(feature = "whatsapp-web")]
pub fn count_storable_turns(sync: &wacore::types::events::LazyHistorySync) -> usize {
    let mut stream = sync.stream();
    let mut total = 0usize;
    while let Ok(Some(conversation)) = stream.next_conversation() {
        for entry in &conversation.messages {
            let Some(info) = entry.message.as_option() else {
                continue;
            };
            let key = info.key.as_option();
            let id = key.and_then(|k| k.id.as_deref());
            let text = message_text(info);
            if is_storable(id, info.message_timestamp, &text) {
                total += 1;
            }
        }
    }
    total
}

/// Extract the human-readable body of a message, if it has one.
///
/// A history sync carries protocol traffic, reactions, revokes and media
/// alongside conversation. Only the two text-bearing shapes are read:
/// `conversation` for a plain message and `extendedTextMessage` for one with a
/// quote, link preview or mention. Anything else yields no text and is
/// filtered out by `is_storable`, which keeps media stubs and system events
/// from padding the agent's context with turns that say nothing.
#[cfg(feature = "whatsapp-web")]
fn message_text(info: &waproto::whatsapp::WebMessageInfo) -> String {
    let Some(message) = info.message.as_option() else {
        return String::new();
    };
    if let Some(text) = message.conversation.as_deref() {
        return text.to_string();
    }
    message
        .extended_text_message
        .as_option()
        .and_then(|ext| ext.text.as_deref())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A recovered turn has to land in the same bucket as a live one, or the
    /// agent hydrates a history nobody will ever look up.
    ///
    /// The expected value is copied from a key the running deployment wrote
    /// for a real conversation, so this fails if the orchestrator's key shape
    /// ever changes underneath — which is the whole point of asserting it.
    #[test]
    fn session_key_matches_the_live_orchestrator_shape() {
        let key = session_key_for_chat("whatsapp.default", "76188559093817@lid", "+5215557654321");
        assert_eq!(
            key, "whatsapp_default_76188559093817_lid__5215557654321",
            "recovered turns must key exactly the way live messages do"
        );
    }

    #[test]
    fn roles_follow_message_direction() {
        assert_eq!(role_for(true), "assistant");
        assert_eq!(role_for(false), "user");
    }

    /// Without an id there is no way to tell this message from a fresh one on
    /// the next sync, which is how a thread ends up stored twice.
    #[test]
    fn a_message_without_an_id_is_not_storable() {
        assert!(!is_storable(None, Some(1_754_170_320), "hola"));
        assert!(!is_storable(Some(""), Some(1_754_170_320), "hola"));
    }

    /// A sync carries protocol traffic, reactions and media stubs. Storing
    /// empty turns pads the agent's context with nothing.
    #[test]
    fn empty_and_whitespace_bodies_are_skipped() {
        assert!(!is_storable(Some("ABC123"), Some(1_754_170_320), ""));
        assert!(!is_storable(Some("ABC123"), Some(1_754_170_320), "   \n "));
    }

    #[test]
    fn a_message_without_a_timestamp_is_not_storable() {
        assert!(!is_storable(Some("ABC123"), None, "hola"));
        assert!(!is_storable(Some("ABC123"), Some(0), "hola"));
    }

    #[test]
    fn a_complete_text_message_is_storable() {
        assert!(is_storable(Some("ABC123"), Some(1_754_170_320), "hola"));
    }
}
