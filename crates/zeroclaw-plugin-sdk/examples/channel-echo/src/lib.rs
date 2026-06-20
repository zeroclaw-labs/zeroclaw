use std::sync::Mutex;

use zeroclaw_plugin_sdk::channel::{
    ChannelCapabilities, ChannelPlugin, InboundMessage, SendMessage,
};

static QUEUE: Mutex<Vec<InboundMessage>> = Mutex::new(Vec::new());
static NEXT_ID: Mutex<u64> = Mutex::new(0);

struct Echo;

impl ChannelPlugin for Echo {
    fn plugin_info() -> (&'static str, &'static str) {
        ("channel-echo", "0.1.0")
    }

    fn name() -> String {
        "channel-echo".to_string()
    }

    fn get_channel_capabilities() -> ChannelCapabilities {
        // Implements no optional capabilities; relies entirely on
        // ChannelPlugin's documented stub defaults.
        ChannelCapabilities::empty()
    }

    fn send(message: SendMessage) -> Result<(), String> {
        let mut next_id = NEXT_ID.lock().map_err(|e| e.to_string())?;
        *next_id += 1;
        let id = next_id.to_string();

        let mut queue = QUEUE.lock().map_err(|e| e.to_string())?;
        queue.push(InboundMessage {
            id,
            sender: "channel-echo".to_string(),
            reply_target: message.recipient.clone(),
            content: message.content,
            channel: "channel-echo".to_string(),
            channel_alias: None,
            timestamp: 0,
            thread_ts: message.thread_ts,
            interruption_scope_id: None,
            attachments: message.attachments,
            subject: message.subject,
        });
        Ok(())
    }

    fn poll_message() -> Option<InboundMessage> {
        let mut queue = QUEUE.lock().ok()?;
        queue.pop()
    }
}

zeroclaw_plugin_sdk::export_channel!(Echo);
