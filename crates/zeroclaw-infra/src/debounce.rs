//! Inbound message debouncing for rapid senders.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use zeroclaw_api::media::MediaAttachment;

/// One inbound message entering a debounce burst.
///
/// A struct rather than positional parameters because the two booleans are
/// adjacent and interchangeable at a call site: passing them in the wrong
/// order would compile cleanly and silently invert whether a burst earns an
/// agent turn.
pub struct BurstPart {
    /// The message body.
    pub content: String,
    /// Media the message carried.
    pub attachments: Vec<MediaAttachment>,
    /// The channel judged this message to be context only — recorded to
    /// history, no agent turn, no visible side effects.
    pub passive_context: bool,
    /// The channel saw a platform-level signal (an @mention) addressing the bot.
    pub explicitly_addressed: bool,
}

impl BurstPart {
    /// A plain text part: no media, no routing signals.
    ///
    /// Exists for tests that exercise timing rather than payload shape, so
    /// adding a field to `BurstPart` does not mean editing every one of them.
    #[must_use]
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            attachments: Vec::new(),
            passive_context: false,
            explicitly_addressed: false,
        }
    }
}

/// The merged payload of one debounce burst.
///
/// Carries the attachments as well as the text because a burst is not
/// text-only: sending a photo and then typing a line about it is two
/// inbound messages, and only one of them holds the image. Merging the
/// text alone would hand the agent a sentence about a picture it was
/// never given.
pub struct DebouncedMessage {
    /// All accumulated message bodies joined with `"\n"`, in arrival order.
    pub content: String,
    /// Every attachment the burst carried, in arrival order. Ownership is
    /// moved out of the inbound messages the debouncer supersedes — the
    /// merged payload is the only surviving copy, never a duplicate of
    /// state still held elsewhere.
    pub attachments: Vec<MediaAttachment>,
    /// The burst is context-only when *every* message in it was. One message
    /// that earned a turn earns one for the whole burst: taking the last
    /// message's flag would let an unrelated line, typed by anyone in the room
    /// within the window, swallow a question actually put to the bot.
    pub passive_context: bool,
    /// True when *any* message in the burst addressed the bot. Being
    /// @mentioned and then adding a follow-up line is still being @mentioned.
    pub explicitly_addressed: bool,
}

struct DebouncerEntry {
    messages: Vec<String>,
    /// Attachments accumulated across the burst, in arrival order.
    attachments: Vec<MediaAttachment>,
    /// Stays true only while every message so far has been passive.
    passive_context: bool,
    /// Latches true on the first message that addressed the bot.
    explicitly_addressed: bool,
    timer_handle: JoinHandle<()>,
    /// Sender for the final merged payload. Replaced on each reset.
    result_tx: Option<tokio::sync::oneshot::Sender<DebouncedMessage>>,
}

/// Result of submitting a message to the debouncer.
pub enum DebounceResult {
    /// The message was accumulated and a timer is running. The caller should
    /// skip processing — the debounced message will arrive via the returned
    /// [`tokio::sync::oneshot::Receiver`] when the window expires.
    Pending(tokio::sync::oneshot::Receiver<DebouncedMessage>),
    /// Debouncing is disabled (window = 0); pass the message through immediately.
    Passthrough(DebouncedMessage),
}

/// Accumulates rapid inbound messages per sender and fires a single combined
/// message after the debounce window elapses without new input.
pub struct MessageDebouncer {
    window: Duration,
    entries: Arc<Mutex<HashMap<String, DebouncerEntry>>>,
}

impl MessageDebouncer {
    /// Create a new debouncer with the given window.
    /// A zero duration disables debouncing (all messages pass through).
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            entries: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Returns `true` when debouncing is active (non-zero window).
    pub fn enabled(&self) -> bool {
        !self.window.is_zero()
    }

    /// Submit a message for debouncing using the debouncer's default window.
    ///
    /// - If the window is zero, returns [`DebounceResult::Passthrough`] immediately.
    /// - Otherwise, accumulates the message under `sender_key` and returns
    ///   [`DebounceResult::Pending`] with a receiver that will eventually yield the
    ///   merged payload once the window expires.
    ///
    /// Attachments are moved into the burst and surface on the merged payload
    /// in arrival order, so a photo sent just before its caption still reaches
    /// the agent attached to the sentence that describes it.
    ///
    /// The routing flags are merged rather than taken from the last message:
    /// `passive_context` survives only if every part was passive, and
    /// `explicitly_addressed` latches on the first part that addressed the bot.
    /// Under `ChannelConversationScope::ReplyTarget` a burst key is room-wide,
    /// so parts can come from *different people* — letting the last arrival
    /// decide would let a stranger's unrelated line silence a question put
    /// directly to the bot.
    ///
    /// Each new message resets the timer. When the timer fires it concatenates all
    /// accumulated messages with `"\n"` and sends them through the oneshot channel.
    pub async fn debounce(&self, sender_key: &str, part: BurstPart) -> DebounceResult {
        self.debounce_inner(sender_key, part, self.window).await
    }

    /// Submit a burst part with an explicit per-call window.
    ///
    /// Same as [`debounce`](Self::debounce) but uses the provided `window`, for
    /// channels that override the global one (e.g. per-alias Telegram config).
    pub async fn debounce_with_window(
        &self,
        sender_key: &str,
        part: BurstPart,
        window: Duration,
    ) -> DebounceResult {
        self.debounce_inner(sender_key, part, window).await
    }

    async fn debounce_inner(
        &self,
        sender_key: &str,
        part: BurstPart,
        window: Duration,
    ) -> DebounceResult {
        let BurstPart {
            content,
            attachments,
            passive_context,
            explicitly_addressed,
        } = part;

        if window.is_zero() {
            return DebounceResult::Passthrough(DebouncedMessage {
                content,
                attachments,
                passive_context,
                explicitly_addressed,
            });
        }

        let mut entries = self.entries.lock().await;
        let entries_ref = Arc::clone(&self.entries);
        let key = sender_key.to_owned();

        if let Some(entry) = entries.get_mut(&key) {
            entry.timer_handle.abort();
            entry.messages.push(content);
            entry.attachments.extend(attachments);
            entry.passive_context &= passive_context;
            entry.explicitly_addressed |= explicitly_addressed;

            let (tx, rx) = tokio::sync::oneshot::channel();
            entry.result_tx = Some(tx);

            entry.timer_handle = zeroclaw_spawn::spawn!(async move {
                tokio::time::sleep(window).await;
                fire_debounced(&entries_ref, &key).await;
            });

            DebounceResult::Pending(rx)
        } else {
            let (tx, rx) = tokio::sync::oneshot::channel();

            let key_clone = key.clone();
            let entries_spawn = Arc::clone(&self.entries);
            let handle = zeroclaw_spawn::spawn!(async move {
                tokio::time::sleep(window).await;
                fire_debounced(&entries_spawn, &key_clone).await;
            });

            entries.insert(
                key,
                DebouncerEntry {
                    messages: vec![content],
                    attachments,
                    passive_context,
                    explicitly_addressed,
                    timer_handle: handle,
                    result_tx: Some(tx),
                },
            );

            DebounceResult::Pending(rx)
        }
    }
}

/// Called when the debounce timer fires. Removes the entry, concatenates all
/// accumulated messages, and sends the merged payload through the oneshot channel.
async fn fire_debounced(entries: &Mutex<HashMap<String, DebouncerEntry>>, key: &str) {
    let mut map = entries.lock().await;
    if let Some(entry) = map.remove(key) {
        let combined = entry.messages.join("\n");
        if let Some(tx) = entry.result_tx {
            let _ = tx.send(DebouncedMessage {
                content: combined,
                attachments: entry.attachments,
                passive_context: entry.passive_context,
                explicitly_addressed: entry.explicitly_addressed,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment(name: &str) -> MediaAttachment {
        MediaAttachment {
            file_name: name.to_owned(),
            data: vec![0u8; 4],
            mime_type: Some("image/jpeg".to_owned()),
        }
    }

    /// An ordinary active message: earns a turn, no explicit mention.
    fn part(content: &str, attachments: Vec<MediaAttachment>) -> BurstPart {
        BurstPart {
            content: content.to_owned(),
            attachments,
            passive_context: false,
            explicitly_addressed: false,
        }
    }

    /// A message the channel judged to be context only.
    fn passive(content: &str) -> BurstPart {
        BurstPart {
            content: content.to_owned(),
            attachments: Vec::new(),
            passive_context: true,
            explicitly_addressed: false,
        }
    }

    /// A message that addressed the bot through a platform signal.
    fn addressed(content: &str) -> BurstPart {
        BurstPart {
            content: content.to_owned(),
            attachments: Vec::new(),
            passive_context: false,
            explicitly_addressed: true,
        }
    }

    #[tokio::test]
    async fn passthrough_when_disabled() {
        let debouncer = MessageDebouncer::new(Duration::ZERO);
        assert!(!debouncer.enabled());
        match debouncer.debounce("user1", part("hello", Vec::new())).await {
            DebounceResult::Passthrough(msg) => assert_eq!(msg.content, "hello"),
            DebounceResult::Pending(_) => panic!("expected Passthrough"),
        }
    }

    #[tokio::test]
    async fn passthrough_keeps_attachments() {
        let debouncer = MessageDebouncer::new(Duration::ZERO);
        match debouncer
            .debounce("user1", part("look", vec![attachment("photo.jpg")]))
            .await
        {
            DebounceResult::Passthrough(msg) => {
                assert_eq!(msg.attachments.len(), 1);
                assert_eq!(msg.attachments[0].file_name, "photo.jpg");
            }
            DebounceResult::Pending(_) => panic!("expected Passthrough"),
        }
    }

    #[tokio::test]
    async fn single_message_fires_after_window() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(50));
        let rx = match debouncer.debounce("user1", part("hello", Vec::new())).await {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };
        let merged = rx.await.unwrap();
        assert_eq!(merged.content, "hello");
    }

    #[tokio::test]
    async fn multiple_messages_concatenated() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(100));

        // First message
        let _rx1 = match debouncer.debounce("user1", part("hello", Vec::new())).await {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };

        tokio::time::sleep(Duration::from_millis(30)).await;
        let rx2 = match debouncer.debounce("user1", part("world", Vec::new())).await {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };

        // The first receiver is dropped (superseded), second gets the combined result
        let merged = rx2.await.unwrap();
        assert_eq!(merged.content, "hello\nworld");
    }

    /// The case this whole payload type exists for: a photo arrives, then the
    /// caption lands inside the debounce window. The photo's message is the one
    /// that gets superseded, so if the merge only carried text the agent would
    /// receive "what do you think of this?" with nothing attached — answering a
    /// question about an image it never saw.
    #[tokio::test]
    async fn attachment_from_superseded_message_survives_the_merge() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(100));

        let _photo = match debouncer
            .debounce("user1", part("", vec![attachment("photo.jpg")]))
            .await
        {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };

        tokio::time::sleep(Duration::from_millis(20)).await;
        let rx = match debouncer
            .debounce("user1", part("what do you think of this?", Vec::new()))
            .await
        {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };

        let merged = rx.await.unwrap();
        assert_eq!(merged.content, "\nwhat do you think of this?");
        assert_eq!(
            merged.attachments.len(),
            1,
            "the photo from the superseded message must reach the agent"
        );
        assert_eq!(merged.attachments[0].file_name, "photo.jpg");
    }

    /// Attachments accumulate across a burst and keep arrival order — several
    /// photos fired back to back are one message with several images, not the
    /// last image only.
    #[tokio::test]
    async fn attachments_accumulate_in_arrival_order() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(120));

        let _first = debouncer
            .debounce("user1", part("a", vec![attachment("one.jpg")]))
            .await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _second = debouncer
            .debounce("user1", part("b", vec![attachment("two.jpg")]))
            .await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let rx = match debouncer
            .debounce("user1", part("c", vec![attachment("three.jpg")]))
            .await
        {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };

        let merged = rx.await.unwrap();
        assert_eq!(merged.content, "a\nb\nc");
        let names: Vec<&str> = merged
            .attachments
            .iter()
            .map(|a| a.file_name.as_str())
            .collect();
        assert_eq!(names, vec!["one.jpg", "two.jpg", "three.jpg"]);
    }

    #[tokio::test]
    async fn different_senders_independent() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(50));

        let rx_a = match debouncer
            .debounce("alice", part("hi alice", Vec::new()))
            .await
        {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };
        let rx_b = match debouncer.debounce("bob", part("hi bob", Vec::new())).await {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };

        assert_eq!(rx_a.await.unwrap().content, "hi alice");
        assert_eq!(rx_b.await.unwrap().content, "hi bob");
    }

    /// Attachments are keyed per sender like text is: Bob's photo must never
    /// surface on Alice's merged message.
    #[tokio::test]
    async fn attachments_do_not_leak_across_senders() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(60));

        let rx_a = match debouncer
            .debounce("alice", part("mine", vec![attachment("alice.jpg")]))
            .await
        {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };
        let rx_b = match debouncer
            .debounce("bob", part("mine too", vec![attachment("bob.jpg")]))
            .await
        {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };

        let a = rx_a.await.unwrap();
        let b = rx_b.await.unwrap();
        assert_eq!(a.attachments.len(), 1);
        assert_eq!(a.attachments[0].file_name, "alice.jpg");
        assert_eq!(b.attachments.len(), 1);
        assert_eq!(b.attachments[0].file_name, "bob.jpg");
    }

    #[tokio::test]
    async fn debounce_with_window_passthrough_when_zero() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(100));
        assert!(debouncer.enabled());
        match debouncer
            .debounce_with_window("user1", BurstPart::text("hello"), Duration::ZERO)
            .await
        {
            DebounceResult::Passthrough(msg) => assert_eq!(msg.content, "hello"),
            DebounceResult::Pending(_) => panic!("expected Passthrough"),
        }
    }

    #[tokio::test]
    async fn debounce_with_window_overrides_default() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(5000)); // long default
        let rx = match debouncer
            .debounce_with_window("user1", BurstPart::text("fast"), Duration::from_millis(50))
            .await
        {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };
        let combined = rx.await.unwrap();
        assert_eq!(combined.content, "fast");
    }

    /// The routing bug this merge exists to prevent. Under a room-wide burst
    /// key, someone asks the bot a question and an unrelated line lands inside
    /// the window. Taking the last message's flag would mark the whole burst as
    /// context-only and the bot would never answer a question put to it.
    #[tokio::test]
    async fn an_active_message_keeps_the_burst_active_even_when_passive_arrives_last() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(100));

        let _asked = debouncer
            .debounce("room", part("bot, status?", Vec::new()))
            .await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let rx = match debouncer
            .debounce("room", passive("unrelated chatter"))
            .await
        {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };

        let merged = rx.await.unwrap();
        assert!(
            !merged.passive_context,
            "a burst containing a message that earned a turn must still earn one"
        );
    }

    /// The converse: a burst where every part was context-only stays passive.
    /// Without this the merge could turn ambient room chatter into agent turns.
    #[tokio::test]
    async fn a_burst_of_only_passive_messages_stays_passive() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(100));

        let _first = debouncer.debounce("room", passive("chatter one")).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let rx = match debouncer.debounce("room", passive("chatter two")).await {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };

        let merged = rx.await.unwrap();
        assert!(
            merged.passive_context,
            "no part earned a turn, so the burst must not earn one"
        );
    }

    /// Being @mentioned and then typing a follow-up line is still being
    /// @mentioned: the signal must not be lost because it arrived first.
    #[tokio::test]
    async fn explicit_address_survives_a_later_unaddressed_message() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(100));

        let _mention = debouncer.debounce("room", addressed("@bot look")).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let rx = match debouncer
            .debounce("room", part("at this", Vec::new()))
            .await
        {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };

        let merged = rx.await.unwrap();
        assert!(
            merged.explicitly_addressed,
            "the @mention from the first part must survive the merge"
        );
        assert_eq!(merged.content, "@bot look\nat this");
    }

    /// A burst nobody addressed to the bot must not acquire the flag.
    #[tokio::test]
    async fn unaddressed_burst_does_not_invent_an_explicit_address() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(100));

        let _first = debouncer.debounce("room", part("one", Vec::new())).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let rx = match debouncer.debounce("room", part("two", Vec::new())).await {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };

        let merged = rx.await.unwrap();
        assert!(!merged.explicitly_addressed);
    }

    /// Passthrough carries the flags through untouched — a disabled debouncer
    /// must not alter routing at all.
    #[tokio::test]
    async fn passthrough_preserves_routing_flags() {
        let debouncer = MessageDebouncer::new(Duration::ZERO);

        match debouncer.debounce("room", passive("ambient")).await {
            DebounceResult::Passthrough(m) => {
                assert!(m.passive_context);
                assert!(!m.explicitly_addressed);
            }
            DebounceResult::Pending(_) => panic!("expected Passthrough"),
        }
        match debouncer.debounce("room", addressed("@bot hi")).await {
            DebounceResult::Passthrough(m) => {
                assert!(!m.passive_context);
                assert!(m.explicitly_addressed);
            }
            DebounceResult::Pending(_) => panic!("expected Passthrough"),
        }
    }
}
