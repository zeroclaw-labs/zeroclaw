use async_trait::async_trait;
use tokio::io::{self, AsyncBufReadExt, BufReader};
use uuid::Uuid;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

/// CLI channel — stdin/stdout, always available, zero deps
pub struct CliChannel {
    alias: String,
}

impl CliChannel {
    pub fn new(alias: impl Into<String>) -> Self {
        Self {
            alias: alias.into(),
        }
    }
}

impl ::zeroclaw_api::attribution::Attributable for CliChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(::zeroclaw_api::attribution::ChannelKind::Cli)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for CliChannel {
    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // The CLI surface has no remote typing indicator to drive.
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // The CLI surface has no remote typing indicator to drive.
        Ok(())
    }

    fn name(&self) -> &str {
        "cli"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        println!("{}", message.content);
        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let _utf8_erase_guard = zeroclaw_runtime::cli_input::ensure_terminal_utf8_erase();
        let stdin = io::stdin();
        let reader = BufReader::new(stdin);
        let mut lines = reader.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            if line == "/quit" || line == "/exit" {
                break;
            }

            let msg = ChannelMessage {
                id: Uuid::new_v4().to_string(),
                sender: "user".to_string(),
                reply_target: "user".to_string(),
                content: line,
                channel: "cli".to_string(),
                channel_alias: None,
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,

                ..Default::default()
            };

            if tx.send(msg).await.is_err() {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_channel_name() {
        assert_eq!(CliChannel::new("cli").name(), "cli");
    }

    #[tokio::test]
    async fn cli_channel_send_does_not_panic() {
        let ch = CliChannel::new("cli");
        let result = ch
            .send(&SendMessage {
                content: "hello".into(),
                recipient: "user".into(),
                subject: None,
                thread_ts: None,
                cancellation_token: None,
                attachments: vec![],
                in_reply_to: None,
                suppress_voice: false,
                force_voice: false,
            })
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn cli_channel_send_empty_message() {
        let ch = CliChannel::new("cli");
        let result = ch
            .send(&SendMessage {
                content: String::new(),
                recipient: String::new(),
                subject: None,
                thread_ts: None,
                cancellation_token: None,
                attachments: vec![],
                in_reply_to: None,
                suppress_voice: false,
                force_voice: false,
            })
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn cli_channel_health_check() {
        let ch = CliChannel::new("cli");
        assert!(ch.health_check().await);
    }

    #[test]
    fn channel_message_struct() {
        let msg = ChannelMessage {
            id: "test-id".into(),
            sender: "user".into(),
            reply_target: "user".into(),
            content: "hello".into(),
            channel: "cli".into(),
            channel_alias: None,
            timestamp: 1_234_567_890,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        };
        assert_eq!(msg.id, "test-id");
        assert_eq!(msg.sender, "user");
        assert_eq!(msg.reply_target, "user");
        assert_eq!(msg.content, "hello");
        assert_eq!(msg.channel, "cli");
        assert_eq!(msg.timestamp, 1_234_567_890);
    }

    #[test]
    fn channel_message_clone() {
        let msg = ChannelMessage {
            id: "id".into(),
            sender: "s".into(),
            reply_target: "s".into(),
            content: "c".into(),
            channel: "ch".into(),
            channel_alias: None,
            timestamp: 0,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        };
        let cloned = msg.clone();
        assert_eq!(cloned.id, msg.id);
        assert_eq!(cloned.content, msg.content);
    }
}
