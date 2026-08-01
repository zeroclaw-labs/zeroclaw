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
    PublishStatus,
    CreatePoll,
    Block,
    Unblock,
    SetDisplayName,
    SetAboutText,
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
        "publish_status",
        "create_poll",
        "block",
        "unblock",
        "set_display_name",
        "set_about_text",
    ];

    /// Actions that address a single message rather than the conversation.
    fn targets_message(self) -> bool {
        matches!(self, Self::Star | Self::Unstar)
    }

    /// Actions that address the account itself, so they carry no chat id.
    fn targets_account(self) -> bool {
        matches!(
            self,
            Self::PublishStatus | Self::SetDisplayName | Self::SetAboutText
        )
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
            "publish_status" => Ok(Self::PublishStatus),
            "create_poll" => Ok(Self::CreatePoll),
            "block" => Ok(Self::Block),
            "unblock" => Ok(Self::Unblock),
            "set_display_name" => Ok(Self::SetDisplayName),
            "set_about_text" => Ok(Self::SetAboutText),
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
                "text": {
                    "type": "string",
                    "description": "Text payload: the status for 'publish_status', the name for 'set_display_name', the about text for 'set_about_text'."
                },
                "question": {
                    "type": "string",
                    "description": "Poll question. Required for 'create_poll'."
                },
                "options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Poll options (2-12). Required for 'create_poll'."
                },
                "selectable_count": {
                    "type": "integer",
                    "description": "How many poll options a voter may pick. Defaults to 1."
                },
                "until_unix_ms": {
                    "type": "integer",
                    "description": "For 'mute': unix milliseconds to stay muted until. Omit to mute indefinitely."
                }
            },
            "required": ["action", "channel"]
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
        // A status is a broadcast, so it has no chat to address; every other
        // action needs one.
        let chat_id = if action.targets_account() {
            ""
        } else {
            required_str(&args, "chat_id")?
        };

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
            ChatAction::PublishStatus => match required_str(&args, "text") {
                Ok(text) => channel.publish_status(text).await.map(|_| ()),
                Err(e) => Err(e),
            },
            ChatAction::CreatePoll => {
                let question = required_str(&args, "question");
                let options: Vec<String> = args
                    .get("options")
                    .and_then(serde_json::Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::trim)
                            .filter(|option| !option.is_empty())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let selectable = args
                    .get("selectable_count")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(1) as u32;
                match question {
                    Ok(question) => channel
                        .create_poll(chat_id, question, &options, selectable)
                        .await
                        .map(|_| ()),
                    Err(e) => Err(e),
                }
            }
            ChatAction::Block => channel.set_contact_blocked(chat_id, true).await,
            ChatAction::Unblock => channel.set_contact_blocked(chat_id, false).await,
            ChatAction::SetDisplayName => match required_str(&args, "text") {
                Ok(name) => channel.set_display_name(name).await,
                Err(e) => Err(e),
            },
            // An empty about text is a legitimate value (it clears the field),
            // so this reads the raw argument instead of requiring non-empty.
            ChatAction::SetAboutText => {
                let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
                channel.set_about_text(text).await
            }
            ChatAction::Star | ChatAction::Unstar => {
                let starred = action == ChatAction::Star;
                let id = message_id.unwrap_or_default();
                channel.set_message_starred(chat_id, &id, starred).await
            }
        };

        Ok(match outcome {
            Ok(()) => ToolResult::ok(if chat_id.is_empty() {
                describe(action).to_string()
            } else {
                format!("{} on {chat_id}", describe(action))
            }),
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
        ChatAction::PublishStatus => "published status",
        ChatAction::CreatePoll => "created poll",
        ChatAction::Block => "blocked",
        ChatAction::Unblock => "unblocked",
        ChatAction::SetDisplayName => "set display name",
        ChatAction::SetAboutText => "set about text",
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
    fn broadcast_actions_do_not_target_a_message() {
        // Star/unstar are the only message-scoped actions; a status or poll
        // must not be routed through the message_id validation path.
        assert!(!ChatAction::PublishStatus.targets_message());
        assert!(!ChatAction::CreatePoll.targets_message());
    }

    #[test]
    fn chat_id_is_only_optional_for_a_status() {
        // A status is a broadcast with no chat to address, but every other
        // action would silently act on the wrong conversation without one.
        assert_eq!(
            ChatAction::SCHEMA_VALUES
                .iter()
                .filter(|value| {
                    value.parse::<ChatAction>().unwrap() == ChatAction::PublishStatus
                })
                .count(),
            1,
            "publish_status must be advertised exactly once"
        );
        assert!(ChatAction::CreatePoll != ChatAction::PublishStatus);
    }

    #[test]
    fn account_scoped_actions_need_no_chat_id() {
        // These address the account itself; demanding a chat id would make
        // them unusable, and accepting one would imply a target they ignore.
        assert!(ChatAction::PublishStatus.targets_account());
        assert!(ChatAction::SetDisplayName.targets_account());
        assert!(ChatAction::SetAboutText.targets_account());
    }

    #[test]
    fn conversation_actions_still_require_a_chat_id() {
        // Blocking without a target would be a no-op at best and a wrong-person
        // block at worst, so these must stay chat-scoped.
        for action in [
            ChatAction::Block,
            ChatAction::Unblock,
            ChatAction::Archive,
            ChatAction::Mute,
            ChatAction::CreatePoll,
        ] {
            assert!(
                !action.targets_account(),
                "{} must require a chat id",
                describe(action)
            );
        }
    }

    #[test]
    fn account_and_message_scopes_never_overlap() {
        // A single action cannot both address the account and a message; the
        // dispatcher would have to guess which target to honour.
        for value in ChatAction::SCHEMA_VALUES {
            let action = value.parse::<ChatAction>().unwrap();
            assert!(
                !(action.targets_account() && action.targets_message()),
                "{value} claims both scopes"
            );
        }
    }

    #[test]
    fn every_action_has_a_distinct_description() {
        // The description is echoed back as the tool's result; a duplicate
        // would make two different outcomes indistinguishable to the agent.
        let mut seen = std::collections::HashSet::new();
        for value in ChatAction::SCHEMA_VALUES {
            let action = value.parse::<ChatAction>().unwrap();
            assert!(
                seen.insert(describe(action)),
                "duplicate description for {value}"
            );
        }
    }

    #[test]
    fn required_str_rejects_blank_values() {
        let args = json!({ "action": "  ", "channel": "whatsapp" });
        assert!(required_str(&args, "action").is_err());
        assert!(required_str(&args, "missing").is_err());
        assert_eq!(required_str(&args, "channel").unwrap(), "whatsapp");
    }
}
