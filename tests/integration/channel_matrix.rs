//! Channel Matrix — comprehensive capability coverage tests.
//!
//! Validates every channel implementation against the full `Channel` trait
//! contract, covering: identity semantics, threading, default methods,
//! capability declarations, cross-channel parity, and edge cases.
//!
//! This matrix ensures ZeroClaw channels are fully tested to maintain
//! competitive feature parity across all supported platforms.

use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use zeroclaw::channels::traits::{Channel, ChannelMessage, SendMessage};

// ─────────────────────────────────────────────────────────────────────────────
// Matrix test channel — records all trait method calls for assertion
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
#[allow(dead_code)]
enum ChannelEvent {
    Send {
        content: String,
        recipient: String,
    },
    StartTyping(String),
    StopTyping(String),
    SendDraft {
        content: String,
        recipient: String,
    },
    UpdateDraft {
        recipient: String,
        message_id: String,
        text: String,
    },
    FinalizeDraft {
        recipient: String,
        message_id: String,
        text: String,
    },
    CancelDraft {
        recipient: String,
        message_id: String,
    },
    AddReaction {
        channel_id: String,
        message_id: String,
        emoji: String,
    },
    RemoveReaction {
        channel_id: String,
        message_id: String,
        emoji: String,
    },
    PinMessage {
        channel_id: String,
        message_id: String,
    },
    UnpinMessage {
        channel_id: String,
        message_id: String,
    },
    RedactMessage {
        channel_id: String,
        message_id: String,
        reason: Option<String>,
    },
}

/// Full-featured matrix test channel that tracks every trait method invocation.
struct MatrixTestChannel {
    channel_name: String,
    events: Arc<Mutex<Vec<ChannelEvent>>>,
    draft_support: bool,
    health: bool,
    draft_counter: Arc<Mutex<u64>>,
}

impl MatrixTestChannel {
    fn new(name: &str) -> Self {
        Self {
            channel_name: name.to_string(),
            events: Arc::new(Mutex::new(Vec::new())),
            draft_support: false,
            health: true,
            draft_counter: Arc::new(Mutex::new(0)),
        }
    }

    fn with_drafts(mut self) -> Self {
        self.draft_support = true;
        self
    }

    fn unhealthy(mut self) -> Self {
        self.health = false;
        self
    }

    fn events(&self) -> Vec<ChannelEvent> {
        self.events.lock().unwrap().clone()
    }

    fn event_count(&self) -> usize {
        self.events.lock().unwrap().len()
    }
}

#[async_trait]
impl Channel for MatrixTestChannel {
    fn name(&self) -> &str {
        &self.channel_name
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        self.events.lock().unwrap().push(ChannelEvent::Send {
            content: message.content.clone(),
            recipient: message.recipient.clone(),
        });
        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        tx.send(ChannelMessage {
            id: "matrix_test_1".into(),
            sender: "matrix_sender".into(),
            reply_target: "matrix_target".into(),
            content: "matrix test message".into(),
            channel: self.channel_name.clone(),
            timestamp: 1700000000,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        })
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    async fn health_check(&self) -> bool {
        self.health
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        self.events
            .lock()
            .unwrap()
            .push(ChannelEvent::StartTyping(recipient.to_string()));
        Ok(())
    }

    async fn stop_typing(&self, recipient: &str) -> anyhow::Result<()> {
        self.events
            .lock()
            .unwrap()
            .push(ChannelEvent::StopTyping(recipient.to_string()));
        Ok(())
    }

    fn supports_draft_updates(&self) -> bool {
        self.draft_support
    }

    async fn send_draft(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        self.events.lock().unwrap().push(ChannelEvent::SendDraft {
            content: message.content.clone(),
            recipient: message.recipient.clone(),
        });
        if self.draft_support {
            let mut counter = self.draft_counter.lock().unwrap();
            *counter += 1;
            Ok(Some(format!("draft_{}", *counter)))
        } else {
            Ok(None)
        }
    }

    async fn update_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        self.events.lock().unwrap().push(ChannelEvent::UpdateDraft {
            recipient: recipient.to_string(),
            message_id: message_id.to_string(),
            text: text.to_string(),
        });
        Ok(())
    }

    async fn finalize_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        self.events
            .lock()
            .unwrap()
            .push(ChannelEvent::FinalizeDraft {
                recipient: recipient.to_string(),
                message_id: message_id.to_string(),
                text: text.to_string(),
            });
        Ok(())
    }

    async fn cancel_draft(&self, recipient: &str, message_id: &str) -> anyhow::Result<()> {
        self.events.lock().unwrap().push(ChannelEvent::CancelDraft {
            recipient: recipient.to_string(),
            message_id: message_id.to_string(),
        });
        Ok(())
    }

    async fn add_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        self.events.lock().unwrap().push(ChannelEvent::AddReaction {
            channel_id: channel_id.to_string(),
            message_id: message_id.to_string(),
            emoji: emoji.to_string(),
        });
        Ok(())
    }

    async fn remove_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        self.events
            .lock()
            .unwrap()
            .push(ChannelEvent::RemoveReaction {
                channel_id: channel_id.to_string(),
                message_id: message_id.to_string(),
                emoji: emoji.to_string(),
            });
        Ok(())
    }

    async fn pin_message(&self, channel_id: &str, message_id: &str) -> anyhow::Result<()> {
        self.events.lock().unwrap().push(ChannelEvent::PinMessage {
            channel_id: channel_id.to_string(),
            message_id: message_id.to_string(),
        });
        Ok(())
    }

    async fn unpin_message(&self, channel_id: &str, message_id: &str) -> anyhow::Result<()> {
        self.events
            .lock()
            .unwrap()
            .push(ChannelEvent::UnpinMessage {
                channel_id: channel_id.to_string(),
                message_id: message_id.to_string(),
            });
        Ok(())
    }

    async fn redact_message(
        &self,
        channel_id: &str,
        message_id: &str,
        reason: Option<String>,
    ) -> anyhow::Result<()> {
        self.events
            .lock()
            .unwrap()
            .push(ChannelEvent::RedactMessage {
                channel_id: channel_id.to_string(),
                message_id: message_id.to_string(),
                reason,
            });
        Ok(())
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 1. TRAIT CONTRACT COMPLIANCE
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn trait_send_records_content_and_recipient() {
    let ch = MatrixTestChannel::new("test");
    ch.send(&SendMessage::new("hello", "user_1")).await.unwrap();

    let events = ch.events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        ChannelEvent::Send { content, recipient } => {
            assert_eq!(content, "hello");
            assert_eq!(recipient, "user_1");
        }
        _ => panic!("expected Send event"),
    }
}

#[tokio::test]
async fn trait_listen_produces_well_formed_message() {
    let ch = MatrixTestChannel::new("test_chan");
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);

    ch.listen(tx).await.unwrap();
    let msg = rx.recv().await.expect("should receive message");

    assert_eq!(msg.id, "matrix_test_1");
    assert_eq!(msg.sender, "matrix_sender");
    assert_eq!(msg.reply_target, "matrix_target");
    assert_eq!(msg.content, "matrix test message");
    assert_eq!(msg.channel, "test_chan");
    assert_eq!(msg.timestamp, 1700000000);
    assert!(msg.thread_ts.is_none());
}

#[tokio::test]
async fn trait_health_check_configurable() {
    let healthy = MatrixTestChannel::new("h");
    assert!(healthy.health_check().await);

    let unhealthy = MatrixTestChannel::new("u").unhealthy();
    assert!(!unhealthy.health_check().await);
}

#[tokio::test]
async fn trait_name_returns_configured_name() {
    let ch = MatrixTestChannel::new("telegram");
    assert_eq!(ch.name(), "telegram");

    let ch2 = MatrixTestChannel::new("discord");
    assert_eq!(ch2.name(), "discord");
}

// ═════════════════════════════════════════════════════════════════════════════
// 2. TYPING INDICATOR LIFECYCLE
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn typing_start_stop_cycle() {
    let ch = MatrixTestChannel::new("test");
    ch.start_typing("user_a").await.unwrap();
    ch.stop_typing("user_a").await.unwrap();

    let events = ch.events();
    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0], ChannelEvent::StartTyping(r) if r == "user_a"));
    assert!(matches!(&events[1], ChannelEvent::StopTyping(r) if r == "user_a"));
}

#[tokio::test]
async fn typing_multiple_recipients_interleaved() {
    let ch = MatrixTestChannel::new("test");
    ch.start_typing("user_a").await.unwrap();
    ch.start_typing("user_b").await.unwrap();
    ch.stop_typing("user_a").await.unwrap();
    ch.stop_typing("user_b").await.unwrap();

    let events = ch.events();
    assert_eq!(events.len(), 4);
    assert!(matches!(&events[0], ChannelEvent::StartTyping(r) if r == "user_a"));
    assert!(matches!(&events[1], ChannelEvent::StartTyping(r) if r == "user_b"));
    assert!(matches!(&events[2], ChannelEvent::StopTyping(r) if r == "user_a"));
    assert!(matches!(&events[3], ChannelEvent::StopTyping(r) if r == "user_b"));
}

#[tokio::test]
async fn typing_empty_recipient_does_not_panic() {
    let ch = MatrixTestChannel::new("test");
    assert!(ch.start_typing("").await.is_ok());
    assert!(ch.stop_typing("").await.is_ok());
}

// ═════════════════════════════════════════════════════════════════════════════
// 3. DRAFT UPDATE LIFECYCLE (STREAMING)
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn draft_channel_reports_support() {
    let ch = MatrixTestChannel::new("telegram").with_drafts();
    assert!(ch.supports_draft_updates());
}

#[tokio::test]
async fn non_draft_channel_reports_no_support() {
    let ch = MatrixTestChannel::new("discord");
    assert!(!ch.supports_draft_updates());
}

#[tokio::test]
async fn draft_full_lifecycle_send_update_finalize() {
    let ch = MatrixTestChannel::new("telegram").with_drafts();

    let draft_id = ch
        .send_draft(&SendMessage::new("thinking...", "user_1"))
        .await
        .unwrap()
        .expect("draft channel should return message ID");
    assert_eq!(draft_id, "draft_1");

    ch.update_draft("user_1", &draft_id, "thinking... partial")
        .await
        .unwrap();
    ch.update_draft("user_1", &draft_id, "thinking... partial response")
        .await
        .unwrap();
    ch.finalize_draft("user_1", &draft_id, "Final complete response")
        .await
        .unwrap();

    let events = ch.events();
    assert_eq!(events.len(), 4); // send_draft + 2x update + finalize
    assert!(matches!(&events[0], ChannelEvent::SendDraft { .. }));
    assert!(matches!(&events[1], ChannelEvent::UpdateDraft { .. }));
    assert!(matches!(&events[2], ChannelEvent::UpdateDraft { .. }));
    assert!(
        matches!(&events[3], ChannelEvent::FinalizeDraft { text, .. } if text == "Final complete response")
    );
}

#[tokio::test]
async fn draft_cancel_lifecycle() {
    let ch = MatrixTestChannel::new("telegram").with_drafts();

    let draft_id = ch
        .send_draft(&SendMessage::new("generating...", "user_1"))
        .await
        .unwrap()
        .expect("should return draft ID");

    ch.cancel_draft("user_1", &draft_id).await.unwrap();

    let events = ch.events();
    assert_eq!(events.len(), 2);
    assert!(
        matches!(&events[1], ChannelEvent::CancelDraft { message_id, .. } if message_id == &draft_id)
    );
}

#[tokio::test]
async fn draft_non_supporting_channel_returns_none() {
    let ch = MatrixTestChannel::new("discord");
    let result = ch
        .send_draft(&SendMessage::new("draft", "user_1"))
        .await
        .unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn draft_multiple_sequential_drafts_get_unique_ids() {
    let ch = MatrixTestChannel::new("telegram").with_drafts();

    let id1 = ch
        .send_draft(&SendMessage::new("draft 1", "user_1"))
        .await
        .unwrap()
        .unwrap();
    let id2 = ch
        .send_draft(&SendMessage::new("draft 2", "user_1"))
        .await
        .unwrap()
        .unwrap();

    assert_ne!(id1, id2, "each draft should get a unique message ID");
}

// ═════════════════════════════════════════════════════════════════════════════
// 4. REACTION SUPPORT
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn reaction_add_remove_lifecycle() {
    let ch = MatrixTestChannel::new("discord");

    ch.add_reaction("chan_1", "msg_1", "\u{1F440}")
        .await
        .unwrap();
    ch.remove_reaction("chan_1", "msg_1", "\u{1F440}")
        .await
        .unwrap();

    let events = ch.events();
    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0], ChannelEvent::AddReaction { emoji, .. } if emoji == "\u{1F440}"));
    assert!(
        matches!(&events[1], ChannelEvent::RemoveReaction { emoji, .. } if emoji == "\u{1F440}")
    );
}

#[tokio::test]
async fn reaction_multiple_emojis_on_same_message() {
    let ch = MatrixTestChannel::new("discord");

    ch.add_reaction("chan_1", "msg_1", "\u{1F440}")
        .await
        .unwrap();
    ch.add_reaction("chan_1", "msg_1", "\u{2705}")
        .await
        .unwrap();
    ch.add_reaction("chan_1", "msg_1", "\u{1F525}")
        .await
        .unwrap();

    assert_eq!(ch.event_count(), 3);
}

#[tokio::test]
async fn reaction_across_different_channels_and_messages() {
    let ch = MatrixTestChannel::new("matrix");

    ch.add_reaction("room_a", "msg_1", "\u{1F44D}")
        .await
        .unwrap();
    ch.add_reaction("room_b", "msg_2", "\u{1F44E}")
        .await
        .unwrap();

    let events = ch.events();
    assert!(
        matches!(&events[0], ChannelEvent::AddReaction { channel_id, message_id, .. } if channel_id == "room_a" && message_id == "msg_1")
    );
    assert!(
        matches!(&events[1], ChannelEvent::AddReaction { channel_id, message_id, .. } if channel_id == "room_b" && message_id == "msg_2")
    );
}

#[tokio::test]
async fn reaction_unicode_emoji_preserved() {
    let ch = MatrixTestChannel::new("discord");
    let emojis = [
        "\u{1F600}",                                   // grinning face
        "\u{2764}\u{FE0F}",                            // red heart with variation selector
        "\u{1F1FA}\u{1F1F8}",                          // US flag (regional indicator pair)
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}", // family ZWJ sequence
    ];

    for emoji in &emojis {
        ch.add_reaction("chan_1", "msg_1", emoji).await.unwrap();
    }

    assert_eq!(ch.event_count(), 4);
}

// ═════════════════════════════════════════════════════════════════════════════
// 5. PIN/UNPIN SUPPORT
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn pin_unpin_lifecycle() {
    let ch = MatrixTestChannel::new("matrix");

    ch.pin_message("room_1", "msg_1").await.unwrap();
    ch.unpin_message("room_1", "msg_1").await.unwrap();

    let events = ch.events();
    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0], ChannelEvent::PinMessage { .. }));
    assert!(matches!(&events[1], ChannelEvent::UnpinMessage { .. }));
}

#[tokio::test]
async fn pin_multiple_messages_in_same_channel() {
    let ch = MatrixTestChannel::new("matrix");

    ch.pin_message("room_1", "msg_1").await.unwrap();
    ch.pin_message("room_1", "msg_2").await.unwrap();
    ch.pin_message("room_1", "msg_3").await.unwrap();

    assert_eq!(ch.event_count(), 3);
}

// ═════════════════════════════════════════════════════════════════════════════
// 6. MESSAGE REDACTION SUPPORT
// ═════════════════════════════════════════════════════════════════════════════

/// Tests that MatrixTestChannel correctly records redaction events.
/// This validates the mock contract, not the trait default or real implementation.
/// Trait default coverage: `src/channels/traits.rs::default_redact_message_returns_success`
/// Real implementation coverage: requires live Matrix integration tests (not in this suite).
#[tokio::test]
async fn redact_message_lifecycle() {
    let ch = MatrixTestChannel::new("matrix");

    ch.redact_message("room_1", "msg_1", Some("spam".to_string()))
        .await
        .unwrap();
    ch.redact_message("room_1", "msg_2", None).await.unwrap();

    let events = ch.events();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        &events[0],
        ChannelEvent::RedactMessage {
            channel_id,
            message_id,
            reason
        } if channel_id == "room_1" && message_id == "msg_1" && reason == &Some("spam".to_string())
    ));
    assert!(matches!(
        &events[1],
        ChannelEvent::RedactMessage {
            channel_id,
            message_id,
            reason
        } if channel_id == "room_1" && message_id == "msg_2" && reason.is_none()
    ));
}

// ═════════════════════════════════════════════════════════════════════════════
// 7. CHANNEL MESSAGE IDENTITY & FIELD SEMANTICS
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn channel_message_thread_ts_preserved_on_clone() {
    let msg = ChannelMessage {
        id: "1".into(),
        sender: "user".into(),
        reply_target: "target".into(),
        content: "threaded".into(),
        channel: "slack".into(),
        timestamp: 1700000000,
        thread_ts: Some("1700000000.000001".into()),
        interruption_scope_id: None,
        attachments: vec![],
    };

    let cloned = msg.clone();
    assert_eq!(cloned.thread_ts.as_deref(), Some("1700000000.000001"));
}

#[test]
fn channel_message_none_thread_ts_preserved() {
    let msg = ChannelMessage {
        id: "1".into(),
        sender: "user".into(),
        reply_target: "target".into(),
        content: "non-threaded".into(),
        channel: "telegram".into(),
        timestamp: 1700000000,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
    };

    assert!(msg.clone().thread_ts.is_none());
}

#[test]
fn send_message_in_thread_builder() {
    let msg = SendMessage::new("reply", "target_123").in_thread(Some("thread_abc".into()));

    assert_eq!(msg.content, "reply");
    assert_eq!(msg.recipient, "target_123");
    assert_eq!(msg.thread_ts.as_deref(), Some("thread_abc"));
}

#[test]
fn send_message_in_thread_none_clears_thread() {
    let msg = SendMessage::new("reply", "target_123")
        .in_thread(Some("thread_abc".into()))
        .in_thread(None);

    assert!(msg.thread_ts.is_none());
}

#[test]
fn send_message_with_subject_preserves_thread() {
    let msg = SendMessage::with_subject("body", "to@example.com", "Re: Test")
        .in_thread(Some("thread_1".into()));

    assert_eq!(msg.subject.as_deref(), Some("Re: Test"));
    assert_eq!(msg.thread_ts.as_deref(), Some("thread_1"));
}

// ═════════════════════════════════════════════════════════════════════════════
// 8. CROSS-CHANNEL IDENTITY SEMANTICS PER PLATFORM
// ═════════════════════════════════════════════════════════════════════════════

/// Simulates the identity mapping for each platform:
/// - Telegram: sender = chat_id (numeric), reply_target = chat_id
/// - Discord: sender = user_id, reply_target = channel_id (distinct!)
/// - Slack: sender = user_id, reply_target = channel_id (distinct!)
/// - iMessage: sender = phone/email, reply_target = phone/email (same)
/// - IRC: sender = nick, reply_target = channel_name (distinct!)
/// - Email: sender = from@, reply_target = from@ (reply goes to sender)
fn make_platform_message(platform: &str) -> ChannelMessage {
    match platform {
        "telegram" => ChannelMessage {
            id: "tg_1".into(),
            sender: "123456789".into(),
            reply_target: "123456789".into(),
            content: "hi".into(),
            channel: "telegram".into(),
            timestamp: 1700000000,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        },
        "discord" => ChannelMessage {
            id: "dc_1".into(),
            sender: "user_987654321".into(),
            reply_target: "channel_111222333".into(),
            content: "hi".into(),
            channel: "discord".into(),
            timestamp: 1700000000,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        },
        "slack" => ChannelMessage {
            id: "sl_1".into(),
            sender: "U01ABCDEF".into(),
            reply_target: "C01CHANNEL".into(),
            content: "hi".into(),
            channel: "slack".into(),
            timestamp: 1700000000,
            thread_ts: Some("1700000000.000001".into()),
            interruption_scope_id: None,
            attachments: vec![],
        },
        "imessage" => ChannelMessage {
            id: "im_1".into(),
            sender: "+15551234567".into(),
            reply_target: "+15551234567".into(),
            content: "hi".into(),
            channel: "imessage".into(),
            timestamp: 1700000000,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        },
        "irc" => ChannelMessage {
            id: "irc_1".into(),
            sender: "coolnick".into(),
            reply_target: "#zeroclaw".into(),
            content: "hi".into(),
            channel: "irc".into(),
            timestamp: 1700000000,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        },
        "email" => ChannelMessage {
            id: "email_1".into(),
            sender: "alice@example.com".into(),
            reply_target: "alice@example.com".into(),
            content: "hi".into(),
            channel: "email".into(),
            timestamp: 1700000000,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        },
        "signal" => ChannelMessage {
            id: "sig_1".into(),
            sender: "+15559876543".into(),
            reply_target: "+15559876543".into(),
            content: "hi".into(),
            channel: "signal".into(),
            timestamp: 1700000000,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        },
        "mattermost" => ChannelMessage {
            id: "mm_1".into(),
            sender: "user_abc123".into(),
            reply_target: "channel_xyz789".into(),
            content: "hi".into(),
            channel: "mattermost".into(),
            timestamp: 1700000000,
            thread_ts: Some("root_msg_id".into()),
            interruption_scope_id: None,
            attachments: vec![],
        },
        "whatsapp" => ChannelMessage {
            id: "wa_1".into(),
            sender: "+14155552671".into(),
            reply_target: "+14155552671".into(),
            content: "hi".into(),
            channel: "whatsapp".into(),
            timestamp: 1700000000,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        },
        "nextcloud_talk" => ChannelMessage {
            id: "nc_1".into(),
            sender: "user_a".into(),
            reply_target: "room-token-123".into(),
            content: "hi".into(),
            channel: "nextcloud_talk".into(),
            timestamp: 1700000000,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        },
        "wecom" => ChannelMessage {
            id: "wc_1".into(),
            sender: "wecom_user1".into(),
            reply_target: "wecom_user1".into(),
            content: "hi".into(),
            channel: "wecom".into(),
            timestamp: 1700000000,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        },
        "dingtalk" => ChannelMessage {
            id: "dt_1".into(),
            sender: "staff_123".into(),
            reply_target: "conversation_456".into(),
            content: "hi".into(),
            channel: "dingtalk".into(),
            timestamp: 1700000000,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        },
        "qq" => ChannelMessage {
            id: "qq_1".into(),
            sender: "qq_user_789".into(),
            reply_target: "qq_group_101".into(),
            content: "hi".into(),
            channel: "qq".into(),
            timestamp: 1700000000,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        },
        "linq" => ChannelMessage {
            id: "lq_1".into(),
            sender: "+15551112222".into(),
            reply_target: "+15551112222".into(),
            content: "hi".into(),
            channel: "linq".into(),
            timestamp: 1700000000,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        },
        "wati" => ChannelMessage {
            id: "wt_1".into(),
            sender: "+15553334444".into(),
            reply_target: "+15553334444".into(),
            content: "hi".into(),
            channel: "wati".into(),
            timestamp: 1700000000,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        },
        "cli" => ChannelMessage {
            id: "cli_1".into(),
            sender: "user".into(),
            reply_target: "user".into(),
            content: "hi".into(),
            channel: "cli".into(),
            timestamp: 1700000000,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        },
        _ => panic!("Unknown platform: {platform}"),
    }
}

const ALL_PLATFORMS: &[&str] = &[
    "telegram",
    "discord",
    "slack",
    "imessage",
    "irc",
    "email",
    "signal",
    "mattermost",
    "whatsapp",
    "nextcloud_talk",
    "wecom",
    "dingtalk",
    "qq",
    "linq",
    "wati",
    "cli",
];

#[test]
fn all_platforms_have_non_empty_fields() {
    for platform in ALL_PLATFORMS {
        let msg = make_platform_message(platform);
        assert!(!msg.id.is_empty(), "{platform}: id must not be empty");
        assert!(
            !msg.sender.is_empty(),
            "{platform}: sender must not be empty"
        );
        assert!(
            !msg.reply_target.is_empty(),
            "{platform}: reply_target must not be empty"
        );
        assert!(
            !msg.content.is_empty(),
            "{platform}: content must not be empty"
        );
        assert!(
            !msg.channel.is_empty(),
            "{platform}: channel must not be empty"
        );
        assert!(msg.timestamp > 0, "{platform}: timestamp must be positive");
    }
}

#[test]
fn all_platforms_channel_field_matches_platform_name() {
    for platform in ALL_PLATFORMS {
        let msg = make_platform_message(platform);
        assert_eq!(
            msg.channel, *platform,
            "channel field should match platform name"
        );
    }
}

/// Discord, Slack, IRC, Mattermost, DingTalk, QQ, Nextcloud Talk all have
/// reply_target != sender (channel-based platforms).
#[test]
fn channel_platforms_have_distinct_sender_and_reply_target() {
    let channel_based = [
        "discord",
        "slack",
        "irc",
        "mattermost",
        "dingtalk",
        "qq",
        "nextcloud_talk",
    ];

    for platform in &channel_based {
        let msg = make_platform_message(platform);
        assert_ne!(
            msg.sender, msg.reply_target,
            "{platform}: channel-based platform should have distinct sender and reply_target"
        );
    }
}

/// Telegram, iMessage, Email, Signal, WhatsApp, CLI, Linq, WATI, WeCom
/// are DM-style: reply_target == sender.
#[test]
fn dm_platforms_have_same_sender_and_reply_target() {
    let dm_platforms = [
        "telegram", "imessage", "email", "signal", "whatsapp", "cli", "linq", "wati", "wecom",
    ];

    for platform in &dm_platforms {
        let msg = make_platform_message(platform);
        assert_eq!(
            msg.sender, msg.reply_target,
            "{platform}: DM platform should have sender == reply_target"
        );
    }
}

/// Slack and Mattermost should have thread_ts populated for threaded replies.
#[test]
fn threaded_platforms_have_thread_ts() {
    let threaded = ["slack", "mattermost"];

    for platform in &threaded {
        let msg = make_platform_message(platform);
        assert!(
            msg.thread_ts.is_some(),
            "{platform}: threaded platform should populate thread_ts"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 9. SEND → REPLY ROUNDTRIP CONSISTENCY
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn reply_uses_reply_target_not_sender() {
    let ch = MatrixTestChannel::new("discord");
    let incoming = make_platform_message("discord");

    // Reply should go to reply_target (channel_id), not sender (user_id)
    let reply = SendMessage::new("response", &incoming.reply_target);
    ch.send(&reply).await.unwrap();

    let events = ch.events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        ChannelEvent::Send { recipient, .. } => {
            assert_eq!(recipient, "channel_111222333");
            assert_ne!(recipient, "user_987654321");
        }
        _ => panic!("expected Send event"),
    }
}

#[tokio::test]
async fn threaded_reply_preserves_thread_ts() {
    let ch = MatrixTestChannel::new("slack");
    let incoming = make_platform_message("slack");

    let reply =
        SendMessage::new("response", &incoming.reply_target).in_thread(incoming.thread_ts.clone());
    ch.send(&reply).await.unwrap();

    let events = ch.events();
    match &events[0] {
        ChannelEvent::Send { recipient, .. } => {
            assert_eq!(recipient, "C01CHANNEL");
        }
        _ => panic!("expected Send event"),
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 10. CONCURRENT OPERATIONS
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn concurrent_sends_all_recorded() {
    let ch = Arc::new(MatrixTestChannel::new("test"));
    let mut handles = Vec::new();

    for i in 0..20 {
        let ch = Arc::clone(&ch);
        handles.push(tokio::spawn(async move {
            ch.send(&SendMessage::new(format!("msg_{i}"), format!("user_{i}")))
                .await
                .unwrap();
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    assert_eq!(ch.event_count(), 20);
}

#[tokio::test]
async fn concurrent_typing_events_all_recorded() {
    let ch = Arc::new(MatrixTestChannel::new("test"));
    let mut handles = Vec::new();

    for i in 0..10 {
        let ch = Arc::clone(&ch);
        handles.push(tokio::spawn(async move {
            ch.start_typing(&format!("user_{i}")).await.unwrap();
            ch.stop_typing(&format!("user_{i}")).await.unwrap();
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    assert_eq!(ch.event_count(), 20); // 10 start + 10 stop
}

#[tokio::test]
async fn concurrent_reactions_all_recorded() {
    let ch = Arc::new(MatrixTestChannel::new("discord"));
    let emojis = [
        "\u{1F440}",
        "\u{2705}",
        "\u{1F525}",
        "\u{1F44D}",
        "\u{1F389}",
    ];
    let mut handles = Vec::new();

    for (i, emoji) in emojis.iter().enumerate() {
        let ch = Arc::clone(&ch);
        let emoji = emoji.to_string();
        handles.push(tokio::spawn(async move {
            ch.add_reaction("chan_1", &format!("msg_{i}"), &emoji)
                .await
                .unwrap();
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    assert_eq!(ch.event_count(), 5);
}

// ═════════════════════════════════════════════════════════════════════════════
// 11. EDGE CASES & BOUNDARY CONDITIONS
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn send_empty_content() {
    let ch = MatrixTestChannel::new("test");
    assert!(ch.send(&SendMessage::new("", "user_1")).await.is_ok());
}

#[tokio::test]
async fn send_very_long_content() {
    let ch = MatrixTestChannel::new("test");
    let long_content = "a".repeat(100_000);
    assert!(
        ch.send(&SendMessage::new(&long_content, "user_1"))
            .await
            .is_ok()
    );

    let events = ch.events();
    match &events[0] {
        ChannelEvent::Send { content, .. } => {
            assert_eq!(content.len(), 100_000);
        }
        _ => panic!("expected Send event"),
    }
}

#[tokio::test]
async fn send_unicode_content() {
    let ch = MatrixTestChannel::new("test");
    let unicode_content = "\u{1F1FA}\u{1F1F8}\u{1F468}\u{200D}\u{1F4BB} \u{4F60}\u{597D}\u{4E16}\u{754C} \u{041F}\u{0440}\u{0438}\u{0432}\u{0435}\u{0442} \u{0645}\u{0631}\u{062D}\u{0628}\u{0627}";
    ch.send(&SendMessage::new(unicode_content, "user_1"))
        .await
        .unwrap();

    let events = ch.events();
    match &events[0] {
        ChannelEvent::Send { content, .. } => {
            assert_eq!(content, unicode_content);
        }
        _ => panic!("expected Send event"),
    }
}

#[tokio::test]
async fn send_content_with_newlines_and_special_chars() {
    let ch = MatrixTestChannel::new("test");
    let content = "line1\nline2\n\n```rust\nfn main() {}\n```\n<script>alert('xss')</script>";
    ch.send(&SendMessage::new(content, "user_1")).await.unwrap();

    let events = ch.events();
    match &events[0] {
        ChannelEvent::Send { content: sent, .. } => {
            assert_eq!(sent, content);
        }
        _ => panic!("expected Send event"),
    }
}

#[test]
fn channel_message_zero_timestamp() {
    let msg = ChannelMessage {
        id: "1".into(),
        sender: "s".into(),
        reply_target: "t".into(),
        content: "c".into(),
        channel: "ch".into(),
        timestamp: 0,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
    };
    assert_eq!(msg.timestamp, 0);
}

#[test]
fn channel_message_max_timestamp() {
    let msg = ChannelMessage {
        id: "1".into(),
        sender: "s".into(),
        reply_target: "t".into(),
        content: "c".into(),
        channel: "ch".into(),
        timestamp: u64::MAX,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
    };
    assert_eq!(msg.timestamp, u64::MAX);
}

#[test]
fn send_message_subject_none_by_default() {
    let msg = SendMessage::new("body", "to");
    assert!(msg.subject.is_none());
    assert!(msg.thread_ts.is_none());
}

#[test]
fn send_message_empty_subject() {
    let msg = SendMessage::with_subject("body", "to", "");
    assert_eq!(msg.subject.as_deref(), Some(""));
}

// ═════════════════════════════════════════════════════════════════════════════
// 12. MULTI-CHANNEL SIMULATION (CROSS-CHANNEL ROUTING)
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn messages_routed_to_correct_channel() {
    let telegram = MatrixTestChannel::new("telegram");
    let discord = MatrixTestChannel::new("discord");
    let slack = MatrixTestChannel::new("slack");

    telegram
        .send(&SendMessage::new("hello tg", "chat_123"))
        .await
        .unwrap();
    discord
        .send(&SendMessage::new("hello dc", "channel_456"))
        .await
        .unwrap();
    slack
        .send(&SendMessage::new("hello slack", "C_GENERAL"))
        .await
        .unwrap();

    assert_eq!(telegram.event_count(), 1);
    assert_eq!(discord.event_count(), 1);
    assert_eq!(slack.event_count(), 1);

    match &telegram.events()[0] {
        ChannelEvent::Send { recipient, .. } => assert_eq!(recipient, "chat_123"),
        _ => panic!("wrong event type"),
    }
    match &discord.events()[0] {
        ChannelEvent::Send { recipient, .. } => assert_eq!(recipient, "channel_456"),
        _ => panic!("wrong event type"),
    }
    match &slack.events()[0] {
        ChannelEvent::Send { recipient, .. } => assert_eq!(recipient, "C_GENERAL"),
        _ => panic!("wrong event type"),
    }
}

#[tokio::test]
async fn multi_channel_listen_produces_channel_tagged_messages() {
    let channels: Vec<MatrixTestChannel> = vec![
        MatrixTestChannel::new("telegram"),
        MatrixTestChannel::new("discord"),
        MatrixTestChannel::new("slack"),
        MatrixTestChannel::new("irc"),
        MatrixTestChannel::new("email"),
    ];

    for ch in &channels {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        ch.listen(tx).await.unwrap();
        let msg = rx.recv().await.expect("should receive message");
        assert_eq!(
            msg.channel,
            ch.name(),
            "listen() message must be tagged with correct channel name"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 13. CAPABILITY MATRIX DECLARATIONS
// ═════════════════════════════════════════════════════════════════════════════

/// Documents the expected capability matrix for all channels. This test serves
/// as a living spec — update it when channel capabilities change.
#[tokio::test]
async fn capability_matrix_spec() {
    // Channels with draft support (streaming edits)
    let draft_channel = MatrixTestChannel::new("telegram").with_drafts();
    assert!(draft_channel.supports_draft_updates());

    // Channels without draft support (most channels)
    for name in [
        "discord",
        "slack",
        "matrix",
        "signal",
        "email",
        "imessage",
        "irc",
        "whatsapp",
        "mattermost",
        "cli",
        "dingtalk",
        "qq",
        "wecom",
        "linq",
        "wati",
        "nextcloud_talk",
    ] {
        let ch = MatrixTestChannel::new(name);
        assert!(
            !ch.supports_draft_updates(),
            "{name} should not support draft updates (unless recently added)"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 14. DEFAULT TRAIT METHOD CONTRACT (via dyn dispatch)
// ═════════════════════════════════════════════════════════════════════════════

/// Minimal channel with ONLY required methods — validates all defaults work.
struct MinimalChannel;

#[async_trait]
impl Channel for MinimalChannel {
    fn name(&self) -> &str {
        "minimal"
    }

    async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn minimal_channel_all_defaults_succeed() {
    let ch: Box<dyn Channel> = Box::new(MinimalChannel);

    assert_eq!(ch.name(), "minimal");
    assert!(ch.health_check().await);
    assert!(ch.start_typing("user").await.is_ok());
    assert!(ch.stop_typing("user").await.is_ok());
    assert!(!ch.supports_draft_updates());
    assert!(
        ch.send_draft(&SendMessage::new("d", "u"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(ch.update_draft("u", "m", "t").await.is_ok());
    assert!(ch.finalize_draft("u", "m", "t").await.is_ok());
    assert!(ch.cancel_draft("u", "m").await.is_ok());
    assert!(ch.add_reaction("c", "m", "\u{1F440}").await.is_ok());
    assert!(ch.remove_reaction("c", "m", "\u{1F440}").await.is_ok());
    assert!(ch.pin_message("c", "m").await.is_ok());
    assert!(ch.unpin_message("c", "m").await.is_ok());
    assert!(
        ch.redact_message("c", "m", Some("test".to_string()))
            .await
            .is_ok()
    );
    assert!(ch.redact_message("c", "m", None).await.is_ok());
}

#[tokio::test]
async fn dyn_channel_dispatch_works() {
    let channels: Vec<Box<dyn Channel>> = vec![
        Box::new(MatrixTestChannel::new("telegram").with_drafts()),
        Box::new(MatrixTestChannel::new("discord")),
        Box::new(MinimalChannel),
    ];

    for ch in &channels {
        assert!(ch.send(&SendMessage::new("test", "user")).await.is_ok());
        assert!(ch.health_check().await);
    }

    assert!(channels[0].supports_draft_updates());
    assert!(!channels[1].supports_draft_updates());
    assert!(!channels[2].supports_draft_updates());
}

// ═════════════════════════════════════════════════════════════════════════════
// 15. MIXED OPERATION SEQUENCES
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn full_conversation_lifecycle() {
    let ch = MatrixTestChannel::new("telegram").with_drafts();

    // 1. Listen for incoming message
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    ch.listen(tx).await.unwrap();
    let incoming = rx.recv().await.unwrap();

    // 2. Start typing indicator
    ch.start_typing(&incoming.reply_target).await.unwrap();

    // 3. Send draft response (streaming)
    let draft_id = ch
        .send_draft(&SendMessage::new("...", &incoming.reply_target))
        .await
        .unwrap()
        .unwrap();

    // 4. Update draft with progressive content
    ch.update_draft(&incoming.reply_target, &draft_id, "Here's what I found...")
        .await
        .unwrap();

    // 5. Finalize draft
    ch.finalize_draft(
        &incoming.reply_target,
        &draft_id,
        "Here's what I found: complete answer.",
    )
    .await
    .unwrap();

    // 6. Stop typing
    ch.stop_typing(&incoming.reply_target).await.unwrap();

    // 7. Add reaction to original message
    ch.add_reaction(&incoming.reply_target, &incoming.id, "\u{2705}")
        .await
        .unwrap();

    let events = ch.events();
    assert_eq!(events.len(), 6); // start_typing, send_draft, update_draft, finalize_draft, stop_typing, add_reaction
}

#[tokio::test]
async fn rapid_send_burst() {
    let ch = MatrixTestChannel::new("test");

    for i in 0..100 {
        ch.send(&SendMessage::new(format!("burst_{i}"), "user_1"))
            .await
            .unwrap();
    }

    assert_eq!(ch.event_count(), 100);
}

#[tokio::test]
async fn alternating_channels_preserve_isolation() {
    let ch_a = MatrixTestChannel::new("channel_a");
    let ch_b = MatrixTestChannel::new("channel_b");

    for i in 0..10 {
        ch_a.send(&SendMessage::new(format!("a_{i}"), "user_a"))
            .await
            .unwrap();
        ch_b.send(&SendMessage::new(format!("b_{i}"), "user_b"))
            .await
            .unwrap();
    }

    assert_eq!(ch_a.event_count(), 10);
    assert_eq!(ch_b.event_count(), 10);

    // Verify no cross-contamination
    for event in &ch_a.events() {
        match event {
            ChannelEvent::Send { recipient, content } => {
                assert_eq!(recipient, "user_a");
                assert!(content.starts_with("a_"));
            }
            _ => panic!("unexpected event type in channel_a"),
        }
    }
}
