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
    /// The message's sender as the live path sees it (`ChannelMessage::sender`).
    ///
    /// Carried separately from `session_key` because the claim key the live
    /// path writes is built from the raw sender, not the derived key. Reusing
    /// that exact shape is what lets an import recognise a message the agent
    /// already answered.
    pub sender: String,
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

/// Extract every storable turn from a history-sync payload.
///
/// Streams the payload conversation by conversation instead of decoding it
/// whole: a sync runs to megabytes and nothing here needs all of it resident.
/// Conversations that fail to decode are skipped rather than aborting the walk
/// — a malformed entry should cost its own turns, not the entire recovered
/// history.
///
/// `resolve_sender` turns a chat JID into the sender string the live path
/// records. This is a parameter rather than something derived here because
/// getting it wrong is silent: the live path stores `+E.164` (resolved through
/// `sender_phone_candidates`, which needs the client's LID→phone mapping), so a
/// JID built locally would key every claim differently and the ledger would
/// treat messages the agent had already answered as brand new. The channel owns
/// that resolution; this module must not guess at it. Returning `None` skips
/// the conversation — better to miss old turns than to import them under a key
/// that defeats the duplicate check.
///
/// Turns come back in the payload's own order. Ordering by timestamp is left
/// to the caller, which has to merge these with whatever the session already
/// holds anyway.
#[cfg(feature = "whatsapp-web")]
pub fn extract_turns<F>(
    sync: &wacore::types::events::LazyHistorySync,
    channel_scope: &str,
    mut resolve_sender: F,
) -> Vec<RecoveredTurn>
where
    F: FnMut(&str) -> Option<String>,
{
    let mut stream = sync.stream();
    let mut turns = Vec::new();
    while let Ok(Some(conversation)) = stream.next_conversation() {
        // Resolved once per conversation: the mapping is per-peer, and a sync
        // can carry hundreds of messages per chat.
        let Some(sender) = resolve_sender(&conversation.id) else {
            continue;
        };
        for entry in &conversation.messages {
            let Some(info) = entry.message.as_option() else {
                continue;
            };
            let Some(key) = info.key.as_option() else {
                continue;
            };
            let text = message_text(info);
            if !is_storable(key.id.as_deref(), info.message_timestamp, &text) {
                continue;
            }
            // Both are Some — is_storable just checked.
            let (Some(message_id), Some(timestamp_secs)) =
                (key.id.as_deref(), info.message_timestamp)
            else {
                continue;
            };
            turns.push(RecoveredTurn {
                message_id: message_id.to_string(),
                sender: sender.clone(),
                session_key: session_key_for_chat(channel_scope, &conversation.id, &sender),
                role: role_for(key.from_me.unwrap_or(false)),
                content: text,
                timestamp_secs,
            });
        }
    }
    turns
}

/// Persist recovered turns into the session store, skipping any already seen.
///
/// Idempotency is delegated to [`ProcessedMessageStore`], which already answers
/// "have I acted on this message" for the live path with an atomic
/// INSERT-OR-IGNORE. Re-deriving that from message content here would be a
/// second copy of the same fact, and the two would drift the moment either
/// changed. The key is built the same way the live path builds it, so a
/// message the agent already answered is not re-imported as history.
///
/// One deliberate difference from the live path: a storage fault means *skip*,
/// not *import*. `claim` fails open because a broken store must not leave the
/// agent mute — answering twice beats never answering. Importing history
/// inverts that trade: failing open would replay an entire conversation into
/// the agent's context, so a fault here costs one missing old turn instead.
///
/// Returns how many turns were newly stored.
#[cfg(feature = "whatsapp-web")]
pub fn persist_turns(
    turns: &[RecoveredTurn],
    channel: &str,
    store: &crate::processed_messages::ProcessedMessageStore,
    session_store: &dyn zeroclaw_infra::session_backend::SessionBackend,
) -> usize {
    use crate::processed_messages::{ClaimOutcome, ProcessedMessageStore};

    let mut stored = 0usize;
    for turn in turns {
        let key = ProcessedMessageStore::key_for(channel, &turn.sender, &turn.message_id);
        if !matches!(store.claim_with_status(&key), ClaimOutcome::Claimed) {
            continue;
        }
        let message = zeroclaw_api::model_provider::ChatMessage {
            role: turn.role.to_string(),
            content: turn.content.clone(),
        };
        if session_store.append(&turn.session_key, &message).is_ok() {
            stored += 1;
        }
    }
    stored
}

/// Count the turns in a history-sync payload that are worth storing.
///
/// Returns a count and nothing else on purpose: it answers "did the platform
/// send usable history, and how much" for a log line, without any message text
/// crossing into a caller that might print it.
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

    /// The property the whole design rests on: importing the same history
    /// twice must not double the conversation. Proven end to end against the
    /// real stores rather than by inspecting the claim logic, because the
    /// guarantee lives in SQLite's uniqueness constraint, not in this code.
    #[cfg(feature = "whatsapp-web")]
    #[test]
    fn re_importing_the_same_turns_stores_nothing_the_second_time() {
        use crate::processed_messages::ProcessedMessageStore;
        use zeroclaw_infra::session_store::SessionStore;

        let dir = tempfile::tempdir().unwrap();
        let claims = ProcessedMessageStore::open_in_state_dir(dir.path()).unwrap();
        let sessions = SessionStore::new(dir.path()).unwrap();

        let turns = vec![
            RecoveredTurn {
                message_id: "MSG_A".into(),
                sender: "76188559093817@lid".into(),
                session_key: "whatsapp_default_chat_peer".into(),
                role: "user",
                content: "hola".into(),
                timestamp_secs: 1_754_170_320,
            },
            RecoveredTurn {
                message_id: "MSG_B".into(),
                sender: "76188559093817@lid".into(),
                session_key: "whatsapp_default_chat_peer".into(),
                role: "assistant",
                content: "hey".into(),
                timestamp_secs: 1_754_170_380,
            },
        ];

        let first = persist_turns(&turns, "whatsapp", &claims, &sessions);
        let second = persist_turns(&turns, "whatsapp", &claims, &sessions);
        let third = persist_turns(&turns, "whatsapp", &claims, &sessions);

        assert_eq!(first, 2, "a fresh import must store every turn");
        assert_eq!(second, 0, "re-importing must store nothing");
        assert_eq!(third, 0, "and must keep storing nothing");
        assert_eq!(
            sessions.load("whatsapp_default_chat_peer").len(),
            2,
            "the conversation must hold one copy of each turn, not three"
        );
    }

    /// A message the agent already answered live must not come back as
    /// history: the live path claims under the same key, so the import sees it
    /// as known. This is why idempotency is delegated to the existing store
    /// instead of a fingerprint derived here.
    ///
    /// The two sides deliberately use the shapes production uses: the live path
    /// claims under the `+E.164` that `sender_phone_candidates` resolves, while
    /// the chat is identified by a LID. An earlier version of this test wrote
    /// the LID on both sides and passed while the real code keyed the two paths
    /// differently — a test that builds both halves from the same assumption
    /// cannot catch a mismatch between them.
    #[cfg(feature = "whatsapp-web")]
    #[test]
    fn a_turn_already_claimed_by_the_live_path_is_not_re_imported() {
        use crate::processed_messages::ProcessedMessageStore;
        use zeroclaw_infra::session_store::SessionStore;

        let dir = tempfile::tempdir().unwrap();
        let claims = ProcessedMessageStore::open_in_state_dir(dir.path()).unwrap();
        let sessions = SessionStore::new(dir.path()).unwrap();

        // What the live path actually writes: the resolved phone number, not
        // the chat's LID.
        let live_sender = "+5215557654321";
        let live_key = ProcessedMessageStore::key_for("whatsapp", live_sender, "MSG_LIVE");
        assert!(
            claims.claim(&live_key),
            "live path claims the message first"
        );

        let session_key = session_key_for_chat(
            "whatsapp.default",
            "76188559093817@lid", // chat JID
            live_sender,          // resolved sender
        );

        let turns = vec![RecoveredTurn {
            message_id: "MSG_LIVE".into(),
            sender: live_sender.into(),
            session_key: session_key.clone(),
            role: "user",
            content: "hola".into(),
            timestamp_secs: 1_754_170_320,
        }];

        assert_eq!(
            persist_turns(&turns, "whatsapp", &claims, &sessions),
            0,
            "a turn the agent already handled must not be imported again"
        );
        assert!(
            sessions.load(&session_key).is_empty(),
            "nothing should have been written"
        );
    }

    /// The failure this module exists to prevent, stated as a test: if the
    /// import keys its claims on the chat JID while the live path keys them on
    /// the resolved phone number, the ledger sees two different messages and
    /// re-imports a conversation the agent already answered.
    #[cfg(feature = "whatsapp-web")]
    #[test]
    fn keying_the_import_on_the_jid_would_defeat_the_duplicate_check() {
        use crate::processed_messages::ProcessedMessageStore;

        let dir = tempfile::tempdir().unwrap();
        let claims = ProcessedMessageStore::open_in_state_dir(dir.path()).unwrap();

        let live = ProcessedMessageStore::key_for("whatsapp", "+5215557654321", "MSG_X");
        let by_jid = ProcessedMessageStore::key_for("whatsapp", "76188559093817@lid", "MSG_X");

        assert_ne!(
            live, by_jid,
            "the two shapes must differ — this is the trap being guarded against"
        );
        assert!(claims.claim(&live), "live path claims first");
        assert!(
            claims.claim(&by_jid),
            "a JID-keyed claim for the SAME message is accepted as new, which is \
             exactly why extract_turns takes a resolver instead of deriving the \
             sender from the chat JID"
        );
    }
}
