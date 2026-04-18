use async_trait::async_trait;

pub use crate::tools::Artifact;

/// A message received from or sent to a channel
#[derive(Debug, Clone)]
pub struct ChannelMessage {
    pub id: String,
    pub sender: String,
    pub reply_target: String,
    pub content: String,
    pub channel: String,
    pub timestamp: u64,
    /// Platform thread identifier (e.g. Slack `ts`, Discord thread ID).
    /// When set, replies should be posted as threaded responses.
    pub thread_ts: Option<String>,
}

/// Message to send through a channel
#[derive(Debug, Clone)]
pub struct SendMessage {
    pub content: String,
    pub recipient: String,
    pub subject: Option<String>,
    /// Platform thread identifier for threaded replies (e.g. Slack `thread_ts`).
    pub thread_ts: Option<String>,
}

impl SendMessage {
    /// Create a new message with content and recipient
    pub fn new(content: impl Into<String>, recipient: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            recipient: recipient.into(),
            subject: None,
            thread_ts: None,
        }
    }

    /// Create a new message with content, recipient, and subject
    pub fn with_subject(
        content: impl Into<String>,
        recipient: impl Into<String>,
        subject: impl Into<String>,
    ) -> Self {
        Self {
            content: content.into(),
            recipient: recipient.into(),
            subject: Some(subject.into()),
            thread_ts: None,
        }
    }

    /// Set the thread identifier for threaded replies.
    pub fn in_thread(mut self, thread_ts: Option<String>) -> Self {
        self.thread_ts = thread_ts;
        self
    }
}

/// Core channel trait — implement for any messaging platform
#[async_trait]
pub trait Channel: Send + Sync {
    /// Human-readable channel name
    fn name(&self) -> &str;

    /// Send a message through this channel
    async fn send(&self, message: &SendMessage) -> anyhow::Result<()>;

    /// Start listening for incoming messages (long-running)
    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()>;

    /// Check if channel is healthy
    async fn health_check(&self) -> bool {
        true
    }

    /// Send a message along with a list of tool-produced file artifacts.
    ///
    /// Default implementation ignores `artifacts` and falls back to
    /// [`send`]. Channels that support native file attachments (Lark
    /// `im/v1/files`, Telegram `sendDocument`, Slack `files.upload`, …)
    /// should override this and upload each artifact inline so the user
    /// sees a proper attachment card rather than a download link.
    ///
    /// The default implementation is intentionally a fallback rather than
    /// a hard `unimplemented!`: this keeps the contract backward compatible
    /// for every existing channel until PR 3 wires native uploads.
    async fn send_with_artifacts(
        &self,
        message: &SendMessage,
        _artifacts: &[Artifact],
    ) -> anyhow::Result<()> {
        self.send(message).await
    }

    /// Draft equivalent of [`send_with_artifacts`].
    ///
    /// Default implementation ignores `artifacts` and falls back to
    /// [`finalize_draft`]. See `send_with_artifacts` for rationale.
    async fn finalize_draft_with_artifacts(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
        _artifacts: &[Artifact],
    ) -> anyhow::Result<()> {
        self.finalize_draft(recipient, message_id, text).await
    }

    /// Signal that the bot is processing a response (e.g. "typing" indicator).
    /// Implementations should repeat the indicator as needed for their platform.
    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// Stop any active typing indicator.
    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// Whether this channel supports progressive message updates via draft edits.
    fn supports_draft_updates(&self) -> bool {
        false
    }

    /// Send an initial draft message. Returns a platform-specific message ID for later edits.
    async fn send_draft(&self, _message: &SendMessage) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    /// Update a previously sent draft message with new accumulated content.
    async fn update_draft(
        &self,
        _recipient: &str,
        _message_id: &str,
        _text: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Finalize a draft with the complete response (e.g. apply Markdown formatting).
    async fn finalize_draft(
        &self,
        _recipient: &str,
        _message_id: &str,
        _text: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Cancel and remove a previously sent draft message if the channel supports it.
    async fn cancel_draft(&self, _recipient: &str, _message_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// Add a reaction (emoji) to a message.
    ///
    /// `channel_id` is the platform channel/conversation identifier (e.g. Discord channel ID).
    /// `message_id` is the platform-scoped message identifier (e.g. `discord_<snowflake>`).
    /// `emoji` is the Unicode emoji to react with (e.g. "👀", "✅").
    async fn add_reaction(
        &self,
        _channel_id: &str,
        _message_id: &str,
        _emoji: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Remove a reaction (emoji) from a message previously added by this bot.
    async fn remove_reaction(
        &self,
        _channel_id: &str,
        _message_id: &str,
        _emoji: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Pin a message in the channel.
    async fn pin_message(&self, _channel_id: &str, _message_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// Unpin a previously pinned message.
    async fn unpin_message(&self, _channel_id: &str, _message_id: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyChannel;

    #[async_trait]
    impl Channel for DummyChannel {
        fn name(&self) -> &str {
            "dummy"
        }

        async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
            Ok(())
        }

        async fn listen(
            &self,
            tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            tx.send(ChannelMessage {
                id: "1".into(),
                sender: "tester".into(),
                reply_target: "tester".into(),
                content: "hello".into(),
                channel: "dummy".into(),
                timestamp: 123,
                thread_ts: None,
            })
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))
        }
    }

    #[test]
    fn channel_message_clone_preserves_fields() {
        let message = ChannelMessage {
            id: "42".into(),
            sender: "alice".into(),
            reply_target: "alice".into(),
            content: "ping".into(),
            channel: "dummy".into(),
            timestamp: 999,
            thread_ts: None,
        };

        let cloned = message.clone();
        assert_eq!(cloned.id, "42");
        assert_eq!(cloned.sender, "alice");
        assert_eq!(cloned.reply_target, "alice");
        assert_eq!(cloned.content, "ping");
        assert_eq!(cloned.channel, "dummy");
        assert_eq!(cloned.timestamp, 999);
    }

    #[tokio::test]
    async fn default_trait_methods_return_success() {
        let channel = DummyChannel;

        assert!(channel.health_check().await);
        assert!(channel.start_typing("bob").await.is_ok());
        assert!(channel.stop_typing("bob").await.is_ok());
        assert!(channel
            .send(&SendMessage::new("hello", "bob"))
            .await
            .is_ok());
    }

    /// PR 2: the default `send_with_artifacts` / `finalize_draft_with_artifacts`
    /// impls must forward to `send` / `finalize_draft`, preserving pre-PR-2
    /// behaviour for every channel that has not yet overridden them.
    #[tokio::test]
    async fn default_artifact_methods_fall_back_to_send() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        struct SendCountingChannel {
            sends: Arc<AtomicUsize>,
            finalize_drafts: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl Channel for SendCountingChannel {
            fn name(&self) -> &str {
                "count"
            }
            async fn send(&self, _m: &SendMessage) -> anyhow::Result<()> {
                self.sends.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
            async fn listen(
                &self,
                _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
            ) -> anyhow::Result<()> {
                Ok(())
            }
            async fn finalize_draft(
                &self,
                _r: &str,
                _id: &str,
                _t: &str,
            ) -> anyhow::Result<()> {
                self.finalize_drafts.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        let sends = Arc::new(AtomicUsize::new(0));
        let finalizes = Arc::new(AtomicUsize::new(0));
        let ch = SendCountingChannel {
            sends: sends.clone(),
            finalize_drafts: finalizes.clone(),
        };

        // Any artifact payload — default impl must ignore it and call send.
        let art = Artifact {
            path: "x.docx".into(),
            name: "x.docx".into(),
            mime: None,
            size_bytes: 1,
            download_url: None,
        };

        ch.send_with_artifacts(&SendMessage::new("hi", "bob"), std::slice::from_ref(&art))
            .await
            .unwrap();
        assert_eq!(sends.load(Ordering::SeqCst), 1);

        ch.finalize_draft_with_artifacts("bob", "m1", "text", &[art])
            .await
            .unwrap();
        assert_eq!(finalizes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn default_reaction_methods_return_success() {
        let channel = DummyChannel;

        assert!(channel
            .add_reaction("chan_1", "msg_1", "\u{1F440}")
            .await
            .is_ok());
        assert!(channel
            .remove_reaction("chan_1", "msg_1", "\u{1F440}")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn default_draft_methods_return_success() {
        let channel = DummyChannel;

        assert!(!channel.supports_draft_updates());
        assert!(channel
            .send_draft(&SendMessage::new("draft", "bob"))
            .await
            .unwrap()
            .is_none());
        assert!(channel.update_draft("bob", "msg_1", "text").await.is_ok());
        assert!(channel
            .finalize_draft("bob", "msg_1", "final text")
            .await
            .is_ok());
        assert!(channel.cancel_draft("bob", "msg_1").await.is_ok());
    }

    #[tokio::test]
    async fn listen_sends_message_to_channel() {
        let channel = DummyChannel;
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);

        channel.listen(tx).await.unwrap();

        let received = rx.recv().await.expect("message should be sent");
        assert_eq!(received.sender, "tester");
        assert_eq!(received.content, "hello");
        assert_eq!(received.channel, "dummy");
    }
}
