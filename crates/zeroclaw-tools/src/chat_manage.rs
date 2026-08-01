//! Conversation-management tool.
//!
//! Reacting to messages is reflexive and belongs on the channel; deciding to
//! archive, mute, pin, or star is volitional and belongs to the agent. This
//! tool exposes that second class of action, late-resolving the channel handle
//! so no chat state is stored here.

use crate::ask_user::ChannelMapHandle;
use async_trait::async_trait;
use serde_json::json;
use std::{
    str::FromStr,
    sync::{Arc, OnceLock},
};
use zeroclaw_api::channel::Channel;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};

const TOOL_NAME: &str = "chat_manage";

static TOOL_DESCRIPTION: OnceLock<String> = OnceLock::new();

fn description() -> &'static str {
    TOOL_DESCRIPTION
        .get_or_init(|| {
            "Manage a conversation the way a person manages their own chat list: \
             archive, mute, pin, mark read/unread, or star a message. \
             Availability depends on the channel; unsupported actions report an error."
                .to_string()
        })
        .as_str()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChatAction {
    Archive,
    Unarchive,
    Mute,
    Unmute,
    Pin,
    Unpin,
    MarkRead,
    MarkUnread,
    Star,
    Unstar,
}

impl ChatAction {
    const SCHEMA_VALUES: &'static [&'static str] = &[
        "archive",
        "unarchive",
        "mute",
        "unmute",
        "pin",
        "unpin",
        "mark_read",
        "mark_unread",
        "star",
        "unstar",
    ];

    /// Actions that address a single message rather than the conversation.
    fn targets_message(self) -> bool {
        matches!(self, Self::Star | Self::Unstar)
    }
}

impl FromStr for ChatAction {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "archive" => Ok(Self::Archive),
            "unarchive" => Ok(Self::Unarchive),
            "mute" => Ok(Self::Mute),
            "unmute" => Ok(Self::Unmute),
            "pin" => Ok(Self::Pin),
            "unpin" => Ok(Self::Unpin),
            "mark_read" => Ok(Self::MarkRead),
            "mark_unread" => Ok(Self::MarkUnread),
            "star" => Ok(Self::Star),
            "unstar" => Ok(Self::Unstar),
            other => anyhow::bail!(
                "unknown action '{other}' (expected one of: {})",
                ChatAction::SCHEMA_VALUES.join(", ")
            ),
        }
    }
}

pub struct ChatManageTool {
    channels: ChannelMapHandle,
    security: Arc<SecurityPolicy>,
}

impl ChatManageTool {
    pub fn new(security: Arc<SecurityPolicy>, channels: ChannelMapHandle) -> Self {
        Self { channels, security }
    }

    fn lookup_channel(&self, channel_name: &str) -> Result<Arc<dyn Channel>, String> {
        let map = self.channels.read();
        if map.is_empty() {
            return Err("no channels are initialized".to_string());
        }
        map.get(channel_name).cloned().ok_or_else(|| {
            let mut available: Vec<String> = map.keys().cloned().collect();
            available.sort();
            format!(
                "unknown channel '{channel_name}' (available: {})",
                available.join(", ")
            )
        })
    }
}

#[async_trait]
impl Tool for ChatManageTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }

    fn description(&self) -> &str {
        description()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ChatAction::SCHEMA_VALUES,
                    "description": "What to do with the conversation or message."
                },
                "channel": {
                    "type": "string",
                    "description": "Channel name, e.g. 'whatsapp'."
                },
                "chat_id": {
                    "type": "string",
                    "description": "Conversation to act on."
                },
                "message_id": {
                    "type": "string",
                    "description": "Message to star/unstar. Required for those actions."
                },
                "until_unix_ms": {
                    "type": "integer",
                    "description": "For 'mute': unix milliseconds to stay muted until. Omit to mute indefinitely."
                }
            },
            "required": ["action", "channel", "chat_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, TOOL_NAME)
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("security policy denied {TOOL_NAME}: {error}")),
            });
        }

        let action = required_str(&args, "action")?;
        let action = ChatAction::from_str(action)?;
        let channel_name = required_str(&args, "channel")?;
        let chat_id = required_str(&args, "chat_id")?;

        let channel = match self.lookup_channel(channel_name) {
            Ok(channel) => channel,
            Err(error) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(error),
                });
            }
        };

        // Resolved before dispatch so a missing id fails with a precise message
        // instead of reaching the channel as an empty target.
        let message_id = if action.targets_message() {
            match args.get("message_id").and_then(|v| v.as_str()) {
                Some(id) if !id.trim().is_empty() => Some(id.trim().to_string()),
                _ => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some("'message_id' is required to star or unstar".to_string()),
                    });
                }
            }
        } else {
            None
        };

        let until = args
            .get("until_unix_ms")
            .and_then(serde_json::Value::as_i64);

        let outcome = match action {
            ChatAction::Archive => channel.set_chat_archived(chat_id, true).await,
            ChatAction::Unarchive => channel.set_chat_archived(chat_id, false).await,
            ChatAction::Mute => channel.set_chat_muted(chat_id, true, until).await,
            ChatAction::Unmute => channel.set_chat_muted(chat_id, false, None).await,
            ChatAction::Pin => channel.set_chat_pinned(chat_id, true).await,
            ChatAction::Unpin => channel.set_chat_pinned(chat_id, false).await,
            ChatAction::MarkRead => channel.set_chat_read(chat_id, true).await,
            ChatAction::MarkUnread => channel.set_chat_read(chat_id, false).await,
            ChatAction::Star | ChatAction::Unstar => {
                let starred = action == ChatAction::Star;
                let id = message_id.unwrap_or_default();
                channel.set_message_starred(chat_id, &id, starred).await
            }
        };

        Ok(match outcome {
            Ok(()) => ToolResult::ok(format!("{} on {chat_id}", describe(action))),
            Err(error) => ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("{} failed: {error}", describe(action))),
            },
        })
    }
}

fn describe(action: ChatAction) -> &'static str {
    match action {
        ChatAction::Archive => "archived",
        ChatAction::Unarchive => "unarchived",
        ChatAction::Mute => "muted",
        ChatAction::Unmute => "unmuted",
        ChatAction::Pin => "pinned",
        ChatAction::Unpin => "unpinned",
        ChatAction::MarkRead => "marked read",
        ChatAction::MarkUnread => "marked unread",
        ChatAction::Star => "starred",
        ChatAction::Unstar => "unstarred",
    }
}

fn required_str<'a>(args: &'a serde_json::Value, key: &str) -> anyhow::Result<&'a str> {
    args.get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::Error::msg(format!("'{key}' is required")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_schema_value_parses_back() {
        // The schema list is what the model is shown; a value it cannot parse
        // would be advertised and then rejected at call time.
        for value in ChatAction::SCHEMA_VALUES {
            assert!(
                value.parse::<ChatAction>().is_ok(),
                "advertised action {value} does not parse"
            );
        }
    }

    #[test]
    fn unknown_action_lists_the_valid_ones() {
        let err = "delete".parse::<ChatAction>().unwrap_err().to_string();
        assert!(err.contains("unknown action 'delete'"));
        assert!(err.contains("archive"), "error should list valid actions");
    }

    #[test]
    fn only_star_actions_target_a_message() {
        assert!(ChatAction::Star.targets_message());
        assert!(ChatAction::Unstar.targets_message());
        assert!(!ChatAction::Archive.targets_message());
        assert!(!ChatAction::Mute.targets_message());
    }

    #[test]
    fn required_str_rejects_blank_values() {
        let args = json!({ "action": "  ", "channel": "whatsapp" });
        assert!(required_str(&args, "action").is_err());
        assert!(required_str(&args, "missing").is_err());
        assert_eq!(required_str(&args, "channel").unwrap(), "whatsapp");
    }
}
