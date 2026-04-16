//! Bridge between WASM plugins and the Channel trait.

use async_trait::async_trait;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

/// A channel backed by a WASM plugin.
pub struct WasmChannel {
    name: String,
    plugin_name: String,
}

impl WasmChannel {
    pub fn new(name: String, plugin_name: String) -> Self {
        Self { name, plugin_name }
    }
}

#[async_trait]
impl Channel for WasmChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        // TODO: Wire to WASM plugin send function
        tracing::warn!(
            "WasmChannel '{}' (plugin: {}) send not yet connected: {}",
            self.name,
            self.plugin_name,
            message.content
        );
        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // TODO: Wire to WASM plugin receive/listen function
        tracing::warn!(
            "WasmChannel '{}' (plugin: {}) listen not yet connected",
            self.name,
            self.plugin_name,
        );
        Ok(())
    }
}
