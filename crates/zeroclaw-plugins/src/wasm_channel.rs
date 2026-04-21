//! Bridge between WASM plugins and the Channel trait.
//!
//! **Status:** Placeholder — `send` and `listen` are not yet wired to the
//! Extism runtime.  Channel plugin support is a Phase 3 (v0.9.0) deliverable
//! per the [Intentional Architecture RFC](https://github.com/zeroclaw-labs/zeroclaw/wiki/14.1-Intentional-Architecture).
//! See `wasm_tool.rs` and `runtime.rs` for the working tool plugin bridge.

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
