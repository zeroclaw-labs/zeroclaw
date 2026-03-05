use crate::channels::traits::{Channel, ChannelMessage, SendMessage};
use crate::config::schema::BridgeConfig;
use async_trait::async_trait;
use tokio::sync::mpsc;

/// Bridge WebSocket channel scaffold.
///
/// This MVP wires config + channel lifecycle into the runtime while the
/// full websocket transport is implemented incrementally.
#[derive(Debug, Clone)]
pub struct BridgeChannel {
    config: BridgeConfig,
}

impl BridgeChannel {
    pub fn new(config: BridgeConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub fn config(&self) -> &BridgeConfig {
        &self.config
    }

    #[must_use]
    pub fn endpoint_url(&self) -> String {
        format!(
            "ws://{}:{}{}",
            self.config.bind_host, self.config.bind_port, self.config.path
        )
    }
}

#[async_trait]
impl Channel for BridgeChannel {
    fn name(&self) -> &str {
        "bridge"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        tracing::info!(
            recipient = %message.recipient,
            subject = ?message.subject,
            bytes = message.content.len(),
            endpoint = %self.endpoint_url(),
            "Bridge channel scaffold send invoked (no-op)"
        );
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        tracing::info!(
            endpoint = %self.endpoint_url(),
            "Bridge channel scaffold listener started (waiting for shutdown)"
        );

        // Keep task alive so supervised listener doesn't hot-restart while
        // websocket transport is being implemented.
        tx.closed().await;
        Ok(())
    }

    async fn health_check(&self) -> bool {
        !self.config.bind_host.trim().is_empty()
            && self.config.bind_host == "127.0.0.1"
            && self.config.bind_port > 0
            && self.config.path.starts_with('/')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_channel_name_and_endpoint_from_config() {
        let channel = BridgeChannel::new(BridgeConfig::default());

        assert_eq!(channel.name(), "bridge");
        assert_eq!(channel.endpoint_url(), "ws://127.0.0.1:8765/ws");
        assert_eq!(channel.config().bind_host, "127.0.0.1");
    }
}
