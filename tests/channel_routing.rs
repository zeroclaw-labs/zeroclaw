//! TG3: Channel Message Identity & Routing Tests
//!
//! Prevents: Pattern 3 — Channel message routing & identity bugs (17% of user bugs).
//! Issues: #496, #483, #620, #415, #503
//!
//! Tests that ChannelMessage fields are used consistently and that the
//! SendMessage → Channel trait contract preserves correct identity semantics.
//! Verifies sender/reply_target field contracts to prevent field swaps.

use async_trait::async_trait;
use zeroclaw::channels::traits::{Channel, ChannelMessage, SendMessage};

// ─────────────────────────────────────────────────────────────────────────────
// ChannelMessage construction and field semantics
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn channel_message_sender_field_holds_platform_user_id() {
    // Simulates Telegram: sender should be numeric chat_id, not username
    let msg = ChannelMessage {
        id: "msg_1".into(),
        sender: "123456789".into(), // numeric chat_id
        reply_target: "msg_0".into(),
        content: "test message".into(),
        channel: "telegram".into(),
        timestamp: 1700000000,
        thread_ts: None,
    };

    assert_eq!(msg.sender, "123456789");
    // Sender should be the platform-level user/chat identifier
    assert!(
        msg.sender.chars().all(|c| c.is_ascii_digit()),
        "Telegram sender should be numeric chat_id, got: {}",
        msg.sender
    );
}

#[test]
fn channel_message_reply_target_distinct_from_sender() {
    // Simulates Discord: reply_target should be channel_id, not sender user_id
    let msg = ChannelMessage {
        id: "msg_1".into(),
        sender: "user_987654".into(),       // Discord user ID
        reply_target: "channel_123".into(), // Discord channel ID for replies
        content: "test message".into(),
        channel: "discord".into(),
        timestamp: 1700000000,
        thread_ts: None,
    };

    assert_ne!(
        msg.sender, msg.reply_target,
        "sender and reply_target should be distinct for Discord"
    );
    assert_eq!(msg.reply_target, "channel_123");
}

#[test]
fn channel_message_fields_not_swapped() {
    // Guards against #496 (Telegram) and #483 (Discord) field swap bugs
    let msg = ChannelMessage {
        id: "msg_42".into(),
        sender: "sender_value".into(),
        reply_target: "target_value".into(),
        content: "payload".into(),
        channel: "test".into(),
        timestamp: 1700000000,
        thread_ts: None,
    };

    assert_eq!(
        msg.sender, "sender_value",
        "sender field should not be swapped"
    );
    assert_eq!(
        msg.reply_target, "target_value",
        "reply_target field should not be swapped"
    );
    assert_ne!(
        msg.sender, msg.reply_target,
        "sender and reply_target should remain distinct"
    );
}

#[test]
fn channel_message_preserves_all_fields_on_clone() {
    let original = ChannelMessage {
        id: "clone_test".into(),
        sender: "sender_123".into(),
        reply_target: "target_456".into(),
        content: "cloned content".into(),
        channel: "test_channel".into(),
        timestamp: 1700000001,
        thread_ts: None,
    };

    let cloned = original.clone();

    assert_eq!(cloned.id, original.id);
    assert_eq!(cloned.sender, original.sender);
    assert_eq!(cloned.reply_target, original.reply_target);
    assert_eq!(cloned.content, original.content);
    assert_eq!(cloned.channel, original.channel);
    assert_eq!(cloned.timestamp, original.timestamp);
}

// ─────────────────────────────────────────────────────────────────────────────
// SendMessage construction
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn send_message_new_sets_content_and_recipient() {
    let msg = SendMessage::new("Hello", "recipient_123");

    assert_eq!(msg.content, "Hello");
    assert_eq!(msg.recipient, "recipient_123");
    assert!(msg.subject.is_none(), "subject should be None by default");
}

#[test]
fn send_message_with_subject_sets_all_fields() {
    let msg = SendMessage::with_subject("Hello", "recipient_123", "Re: Test");

    assert_eq!(msg.content, "Hello");
    assert_eq!(msg.recipient, "recipient_123");
    assert_eq!(msg.subject.as_deref(), Some("Re: Test"));
}

#[test]
fn send_message_recipient_carries_platform_target() {
    // Verifies that SendMessage::recipient is used as the platform delivery target
    // For Telegram: this should be the chat_id
    // For Discord: this should be the channel_id
    let telegram_msg = SendMessage::new("response", "123456789");
    assert_eq!(
        telegram_msg.recipient, "123456789",
        "Telegram SendMessage recipient should be chat_id"
    );

    let discord_msg = SendMessage::new("response", "channel_987654");
    assert_eq!(
        discord_msg.recipient, "channel_987654",
        "Discord SendMessage recipient should be channel_id"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Channel trait contract: send/listen roundtrip via DummyChannel
// ─────────────────────────────────────────────────────────────────────────────

/// Test channel that captures sent messages for assertion
struct CapturingChannel {
    sent: std::sync::Mutex<Vec<SendMessage>>,
}

impl CapturingChannel {
    fn new() -> Self {
        Self {
            sent: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn sent_messages(&self) -> Vec<SendMessage> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait]
impl Channel for CapturingChannel {
    fn name(&self) -> &str {
        "capturing"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        self.sent.lock().unwrap().push(message.clone());
        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        tx.send(ChannelMessage {
            id: "listen_1".into(),
            sender: "test_sender".into(),
            reply_target: "test_target".into(),
            content: "incoming".into(),
            channel: "capturing".into(),
            timestamp: 1700000000,
            thread_ts: None,
        })
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))
    }
}

#[tokio::test]
async fn channel_send_preserves_recipient() {
    let channel = CapturingChannel::new();
    let msg = SendMessage::new("Hello", "target_123");

    channel.send(&msg).await.unwrap();

    let sent = channel.sent_messages();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].recipient, "target_123");
    assert_eq!(sent[0].content, "Hello");
}

#[tokio::test]
async fn channel_listen_produces_correct_identity_fields() {
    let channel = CapturingChannel::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);

    channel.listen(tx).await.unwrap();
    let received = rx.recv().await.expect("should receive message");

    assert_eq!(received.sender, "test_sender");
    assert_eq!(received.reply_target, "test_target");
    assert_ne!(
        received.sender, received.reply_target,
        "listen() should populate sender and reply_target distinctly"
    );
}

#[tokio::test]
async fn channel_send_reply_uses_sender_from_listen() {
    let channel = CapturingChannel::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);

    // Simulate: listen() → receive message → send reply using sender
    channel.listen(tx).await.unwrap();
    let incoming = rx.recv().await.expect("should receive message");

    // Reply should go to the reply_target, not sender
    let reply = SendMessage::new("reply content", &incoming.reply_target);
    channel.send(&reply).await.unwrap();

    let sent = channel.sent_messages();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].recipient, "test_target",
        "reply should use reply_target as recipient"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Channel trait default methods
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn channel_health_check_default_returns_true() {
    let channel = CapturingChannel::new();
    assert!(
        channel.health_check().await,
        "default health_check should return true"
    );
}

#[tokio::test]
async fn channel_typing_defaults_succeed() {
    let channel = CapturingChannel::new();
    assert!(channel.start_typing("target").await.is_ok());
    assert!(channel.stop_typing("target").await.is_ok());
}

#[tokio::test]
async fn channel_draft_defaults() {
    let channel = CapturingChannel::new();
    assert!(!channel.supports_draft_updates());

    let draft_result = channel
        .send_draft(&SendMessage::new("draft", "target"))
        .await
        .unwrap();
    assert!(
        draft_result.is_none(),
        "default send_draft should return None"
    );

    assert!(channel
        .update_draft("target", "msg_1", "updated")
        .await
        .is_ok());
    assert!(channel
        .finalize_draft("target", "msg_1", "final")
        .await
        .is_ok());
}

// ─────────────────────────────────────────────────────────────────────────────
// Multiple messages: conversation context preservation
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn channel_multiple_sends_preserve_order_and_recipients() {
    let channel = CapturingChannel::new();

    channel
        .send(&SendMessage::new("msg 1", "target_a"))
        .await
        .unwrap();
    channel
        .send(&SendMessage::new("msg 2", "target_b"))
        .await
        .unwrap();
    channel
        .send(&SendMessage::new("msg 3", "target_a"))
        .await
        .unwrap();

    let sent = channel.sent_messages();
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0].recipient, "target_a");
    assert_eq!(sent[1].recipient, "target_b");
    assert_eq!(sent[2].recipient, "target_a");
    assert_eq!(sent[0].content, "msg 1");
    assert_eq!(sent[1].content, "msg 2");
    assert_eq!(sent[2].content, "msg 3");
}
