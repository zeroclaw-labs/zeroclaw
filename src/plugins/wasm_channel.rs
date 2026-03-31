//! Bridge between WASM plugins and the Channel trait.

use crate::channels::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use extism::Plugin;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

/// JSON DTO sent to the `channel_send` WASM export.
#[derive(Serialize)]
struct WasmSendInput<'a> {
    content: &'a str,
    recipient: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    subject: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_ts: Option<&'a str>,
}

/// JSON DTO returned by the `channel_send` WASM export.
#[derive(Deserialize)]
struct WasmSendResponse {
    success: bool,
    error: Option<String>,
}

/// A single message returned by the `channel_listen` WASM export.
#[derive(Deserialize)]
struct WasmChannelMessageDto {
    id: String,
    sender: String,
    reply_target: String,
    content: String,
    channel: String,
    timestamp: u64,
    thread_ts: Option<String>,
    interruption_scope_id: Option<String>,
}

/// JSON DTO returned by the `channel_listen` WASM export.
#[derive(Deserialize)]
struct WasmListenResponse {
    messages: Vec<WasmChannelMessageDto>,
}

/// A channel backed by a WASM plugin.
pub struct WasmChannel {
    name: String,
    plugin_name: String,
    plugin: Arc<Mutex<Plugin>>,
}

impl WasmChannel {
    pub fn new(name: String, plugin_name: String, plugin: Arc<Mutex<Plugin>>) -> Self {
        Self {
            name,
            plugin_name,
            plugin,
        }
    }
}

#[async_trait]
impl Channel for WasmChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let input = WasmSendInput {
            content: &message.content,
            recipient: &message.recipient,
            subject: message.subject.as_deref(),
            thread_ts: message.thread_ts.as_deref(),
        };
        let json_bytes = serde_json::to_vec(&input)?;

        let mut plugin = match self.plugin.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        let output_bytes = plugin
            .call::<&[u8], &[u8]>("channel_send", &json_bytes)
            .map_err(|e| {
                anyhow::anyhow!(
                    "WasmChannel '{}' (plugin: {}) channel_send failed: {}",
                    self.name,
                    self.plugin_name,
                    e
                )
            })?;

        let response: WasmSendResponse = serde_json::from_slice(output_bytes).map_err(|e| {
            anyhow::anyhow!(
                "WasmChannel '{}' (plugin: {}) channel_send returned invalid JSON: {}",
                self.name,
                self.plugin_name,
                e
            )
        })?;

        if !response.success {
            return Err(anyhow::anyhow!(
                "WasmChannel '{}' (plugin: {}) channel_send error: {}",
                self.name,
                self.plugin_name,
                response.error.unwrap_or_else(|| "unknown error".into())
            ));
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // Call the WASM export while holding the lock, then drop before awaiting.
        let messages = {
            let mut plugin = match self.plugin.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };

            let output_bytes = plugin
                .call::<&[u8], &[u8]>("channel_listen", b"{}")
                .map_err(|e| {
                    anyhow::anyhow!(
                        "WasmChannel '{}' (plugin: {}) channel_listen failed: {}",
                        self.name,
                        self.plugin_name,
                        e
                    )
                })?;

            let response: WasmListenResponse =
                serde_json::from_slice(output_bytes).map_err(|e| {
                    anyhow::anyhow!(
                        "WasmChannel '{}' (plugin: {}) channel_listen returned invalid JSON: {}",
                        self.name,
                        self.plugin_name,
                        e
                    )
                })?;

            response.messages
        }; // MutexGuard dropped here

        for msg in messages {
            let channel_msg = ChannelMessage {
                id: msg.id,
                sender: msg.sender,
                reply_target: msg.reply_target,
                content: msg.content,
                channel: msg.channel,
                timestamp: msg.timestamp,
                thread_ts: msg.thread_ts,
                interruption_scope_id: msg.interruption_scope_id,
                attachments: vec![],
            };

            tx.send(channel_msg)
                .await
                .map_err(|e| anyhow::anyhow!("failed to forward channel message: {}", e))?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create an `Arc<Mutex<Plugin>>` from a minimal empty WASM module.
    fn make_test_plugin() -> Arc<Mutex<Plugin>> {
        let wasm_bytes: &[u8] = &[
            0x00, 0x61, 0x73, 0x6d, // \0asm
            0x01, 0x00, 0x00, 0x00, // version 1
        ];
        let manifest = extism::Manifest::new([extism::Wasm::data(wasm_bytes)]);
        let plugin = Plugin::new(&manifest, [], true).expect("minimal wasm should load");
        Arc::new(Mutex::new(plugin))
    }

    #[test]
    fn wasm_channel_exposes_name() {
        let ch = WasmChannel::new("test-chan".into(), "test-plugin".into(), make_test_plugin());
        assert_eq!(ch.name(), "test-chan");
    }

    #[tokio::test]
    async fn send_errors_on_missing_export() {
        let ch = WasmChannel::new("test-chan".into(), "test-plugin".into(), make_test_plugin());
        let msg = SendMessage::new("hello", "alice");
        let result = ch.send(&msg).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("channel_send"));
    }

    #[tokio::test]
    async fn listen_errors_on_missing_export() {
        let ch = WasmChannel::new("test-chan".into(), "test-plugin".into(), make_test_plugin());
        let (tx, _rx) = tokio::sync::mpsc::channel(10);
        let result = ch.listen(tx).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("channel_listen"));
    }
}
