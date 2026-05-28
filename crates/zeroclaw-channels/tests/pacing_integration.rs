//! Integration coverage for `reply_min_interval_secs` + bounded queue
//! against the recipient shapes used by Telegram and WhatsApp Web — the
//! two channels #6345 calls out by name.
//!
//! These tests live at the pacing layer rather than inside each channel's
//! HTTP/WS protocol mocks because pacing is a `PacedChannel` wrapper
//! concern, not channel-internal logic. The recipient-key shape
//! (Telegram numeric chat_id, WA Web LID/JID) is exercised explicitly so
//! the assertion is anchored to the same identifier the production
//! channels would receive.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use parking_lot::Mutex;
use zeroclaw_api::attribution::{Attributable, ChannelKind, Role};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_channels::paced_channel::PacedChannel;
use zeroclaw_config::schema::HasReplyPacing;

/// Minimal `HasReplyPacing` test fixture mirroring the production trait
/// shape. Avoids dragging a full `TelegramConfig` / `WhatsAppConfig`
/// literal into every test case.
struct PacingFixture {
    interval_secs: u64,
    depth: u16,
}
impl HasReplyPacing for PacingFixture {
    fn reply_min_interval_secs(&self) -> u64 {
        self.interval_secs
    }
    fn reply_queue_depth_max(&self) -> u16 {
        self.depth
    }
}

/// Records every recipient + content + send instant so the test can
/// assert ordering and cadence end-to-end. Stands in for the inner
/// `TelegramChannel` / `WhatsAppWebChannel` so the pacing wrapper sees
/// the exact wire shape the production channels would.
struct RecordingChannel {
    alias: &'static str,
    events: Arc<Mutex<Vec<(String, String, Instant)>>>,
    sends: AtomicUsize,
}

impl Attributable for RecordingChannel {
    fn role(&self) -> Role {
        Role::Channel(ChannelKind::Cli)
    }
    fn alias(&self) -> &str {
        self.alias
    }
}

#[async_trait]
impl Channel for RecordingChannel {
    fn name(&self) -> &str {
        self.alias
    }
    async fn send(&self, message: &SendMessage) -> Result<()> {
        self.events.lock().push((
            message.recipient.clone(),
            message.content.clone(),
            Instant::now(),
        ));
        self.sends.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        Ok(())
    }
}

/// Telegram-shaped recipient: numeric chat_id string. Asserts the pacing
/// floor holds between consecutive replies to the same chat_id, and that
/// a different chat_id is independent.
#[tokio::test]
async fn telegram_shape_pacing_floor_holds_between_consecutive_sends() {
    let events: Arc<Mutex<Vec<(String, String, Instant)>>> = Arc::new(Mutex::new(Vec::new()));
    let inner: Arc<dyn Channel> = Arc::new(RecordingChannel {
        alias: "telegram_test",
        events: Arc::clone(&events),
        sends: AtomicUsize::new(0),
    });
    let cfg = PacingFixture {
        interval_secs: 1,
        depth: 8,
    };
    let paced = PacedChannel::wrap(inner, &cfg);

    // Telegram chat_ids are numeric (positive for users, negative for
    // groups). Mix both to make sure recipient-key hashing handles them.
    let chat_user = "123456789";
    let chat_group = "-987654321";

    let started = Instant::now();
    paced
        .send(&SendMessage::new("hi user", chat_user))
        .await
        .unwrap();
    paced
        .send(&SendMessage::new("hi user again", chat_user))
        .await
        .unwrap();
    paced
        .send(&SendMessage::new("hi group", chat_group))
        .await
        .unwrap();
    let elapsed = started.elapsed();

    // Three sends total. The two same-chat sends must be ≥1s apart; the
    // group send fires on its own floor (started at 0) so it's effectively
    // immediate. Total elapsed ≥ 1s (the second user send waits) but well
    // under 2s (the group send isn't blocked).
    assert!(
        elapsed >= Duration::from_millis(900),
        "second user send should observe pacing floor; total elapsed = {elapsed:?}",
    );
    assert!(
        elapsed < Duration::from_millis(1900),
        "group send must not be blocked by user's floor; total elapsed = {elapsed:?}",
    );

    let log = events.lock();
    assert_eq!(log.len(), 3, "all three sends must reach inner channel");
    // Recipient ordering: user, user, group (group fired between the two
    // user sends but the recording reflects what the inner channel sees,
    // which is dispatch order).
    let recipients: Vec<&str> = log.iter().map(|(r, _, _)| r.as_str()).collect();
    assert!(recipients.contains(&chat_user));
    assert!(recipients.contains(&chat_group));

    // Verify the same-recipient timestamps are spaced by ≥ 1s.
    let user_sends: Vec<Instant> = log
        .iter()
        .filter(|(r, _, _)| r == chat_user)
        .map(|(_, _, t)| *t)
        .collect();
    assert_eq!(user_sends.len(), 2);
    let gap = user_sends[1].duration_since(user_sends[0]);
    assert!(
        gap >= Duration::from_millis(900),
        "consecutive user-chat sends must respect the floor; gap = {gap:?}",
    );
}

/// WhatsApp Web-shaped recipient: JID with `@s.whatsapp.net` suffix.
/// Asserts the pacing floor + queue overflow contract on a recipient
/// shape Audacity flagged in #6622 (LID vs phone reconciliation lives
/// in a different layer; pacing operates on whatever recipient string
/// the inner channel receives).
#[tokio::test]
async fn whatsapp_web_shape_queue_overflow_drops_newest() {
    let events: Arc<Mutex<Vec<(String, String, Instant)>>> = Arc::new(Mutex::new(Vec::new()));
    let counting = Arc::new(RecordingChannel {
        alias: "whatsapp_web_test",
        events: Arc::clone(&events),
        sends: AtomicUsize::new(0),
    });
    let inner: Arc<dyn Channel> = counting.clone();
    let cfg = PacingFixture {
        interval_secs: 1,
        depth: 2,
    };
    let paced = PacedChannel::wrap(inner, &cfg);

    let jid = "[email protected]";

    // Immediate send fires and starts the floor (~1s out).
    paced.send(&SendMessage::new("first", jid)).await.unwrap();
    // Spawn two queued sends so they actually push into the queue
    // before the overflow attempt.
    let paced_a = Arc::clone(&paced);
    let paced_b = Arc::clone(&paced);
    let h_a = tokio::spawn(async move { paced_a.send(&SendMessage::new("a", jid)).await });
    let h_b = tokio::spawn(async move { paced_b.send(&SendMessage::new("b", jid)).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Overflow — queue is full (depth=2). Drop newest, returns Ok.
    paced
        .send(&SendMessage::new("overflow", jid))
        .await
        .unwrap();
    let (a, b) = tokio::join!(h_a, h_b);
    a.unwrap().unwrap();
    b.unwrap().unwrap();

    // 1 immediate + 2 queued = 3 dispatches; overflow must not reach
    // the inner channel.
    assert_eq!(
        counting.sends.load(Ordering::SeqCst),
        3,
        "queue overflow must drop the newest send before the inner channel sees it",
    );

    let log = events.lock();
    let contents: Vec<&str> = log.iter().map(|(_, c, _)| c.as_str()).collect();
    assert!(contents.contains(&"first"));
    assert!(contents.contains(&"a"));
    assert!(contents.contains(&"b"));
    assert!(!contents.contains(&"overflow"));
}

/// Zero interval is a true passthrough — verified at the integration
/// layer so the assertion holds whether the inner channel is Telegram,
/// WhatsApp Web, or any other `Channel` impl.
#[tokio::test]
async fn zero_interval_passthrough_at_integration_layer() {
    let events: Arc<Mutex<Vec<(String, String, Instant)>>> = Arc::new(Mutex::new(Vec::new()));
    let counting = Arc::new(RecordingChannel {
        alias: "passthrough_test",
        events: Arc::clone(&events),
        sends: AtomicUsize::new(0),
    });
    let inner: Arc<dyn Channel> = counting.clone();
    let cfg = PacingFixture {
        interval_secs: 0,
        depth: 0,
    };
    let paced = PacedChannel::wrap(Arc::clone(&inner), &cfg);

    // When interval == 0, wrap() returns the inner Arc unchanged. The
    // identity check is the cheapest assertion that no wrapper allocated.
    assert!(
        Arc::ptr_eq(&paced, &inner),
        "zero-interval pacing must return the inner Arc unchanged",
    );

    // Three rapid-fire sends should complete in well under one floor
    // interval since there is no wrapper.
    let started = Instant::now();
    for n in 0..3 {
        paced
            .send(&SendMessage::new(&format!("msg-{n}"), "any"))
            .await
            .unwrap();
    }
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "passthrough path must not introduce any pacing delay",
    );
    assert_eq!(counting.sends.load(Ordering::SeqCst), 3);
}
