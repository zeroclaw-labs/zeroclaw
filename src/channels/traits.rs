use async_trait::async_trait;

/// A message received from or sent to a channel
#[derive(Debug, Clone)]
pub struct ChannelMessage {
    pub id: String,
    pub sender: String,
    pub content: String,
    pub channel: String,
    pub timestamp: u64,
}

/// Core channel trait — implement for any messaging platform
#[async_trait]
pub trait Channel: Send + Sync {
    /// Human-readable channel name
    fn name(&self) -> &str;

    /// Send a message through this channel
    async fn send(&self, message: &str, recipient: &str) -> anyhow::Result<()>;

    /// Start listening for incoming messages (long-running)
    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()>;

    /// Check if channel is healthy
    async fn health_check(&self) -> bool {
        true
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

        async fn send(&self, _message: &str, _recipient: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn listen(
            &self,
            tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            tx.send(ChannelMessage {
                id: "1".into(),
                sender: "tester".into(),
                content: "hello".into(),
                channel: "dummy".into(),
                timestamp: 123,
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
            content: "ping".into(),
            channel: "dummy".into(),
            timestamp: 999,
        };

        let cloned = message.clone();
        assert_eq!(cloned.id, "42");
        assert_eq!(cloned.sender, "alice");
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
        assert!(channel.send("hello", "bob").await.is_ok());
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
