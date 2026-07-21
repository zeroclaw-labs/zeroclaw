//! Channel room-management tool.

use crate::ask_user::ChannelMapHandle;
use async_trait::async_trait;
use serde_json::json;
use std::{
    str::FromStr,
    sync::{Arc, OnceLock},
};
use zeroclaw_api::channel::{Channel, RoomCreationOptions, RoomVisibility};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};

const TOOL_DESCRIPTION_KEY: &str = "tool-channel-room";
static TOOL_DESCRIPTION: OnceLock<String> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelRoomAction {
    CreateRoom,
    InviteUser,
}

impl ChannelRoomAction {
    const CREATE_ROOM: &'static str = "create_room";
    const INVITE_USER: &'static str = "invite_user";
    const SCHEMA_VALUES: &'static [&'static str] = &[Self::CREATE_ROOM, Self::INVITE_USER];

    fn as_str(self) -> &'static str {
        match self {
            Self::CreateRoom => Self::CREATE_ROOM,
            Self::InviteUser => Self::INVITE_USER,
        }
    }
}

impl FromStr for ChannelRoomAction {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            Self::CREATE_ROOM => Ok(Self::CreateRoom),
            Self::INVITE_USER => Ok(Self::InviteUser),
            other => {
                let action = other.to_string();
                anyhow::bail!(tool_msg_with_args(
                    "tool-channel-room-error-invalid-action",
                    &[("action", &action)]
                ))
            }
        }
    }
}

pub struct ChannelRoomTool {
    channels: ChannelMapHandle,
    security: Arc<SecurityPolicy>,
}

impl ChannelRoomTool {
    pub fn new(security: Arc<SecurityPolicy>, channels: ChannelMapHandle) -> Self {
        Self { channels, security }
    }
}

#[async_trait]
impl Tool for ChannelRoomTool {
    fn name(&self) -> &str {
        "channel_room"
    }

    fn description(&self) -> &str {
        TOOL_DESCRIPTION
            .get_or_init(|| crate::i18n::get_required_tool_string(TOOL_DESCRIPTION_KEY))
            .as_str()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ChannelRoomAction::SCHEMA_VALUES,
                    "description": tool_msg("tool-channel-room-param-action")
                },
                "channel": {
                    "type": "string",
                    "description": tool_msg("tool-channel-room-param-channel")
                },
                "name": {
                    "type": "string",
                    "description": tool_msg("tool-channel-room-param-name")
                },
                "topic": {
                    "type": "string",
                    "description": tool_msg("tool-channel-room-param-topic")
                },
                "invites": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": tool_msg("tool-channel-room-param-invites")
                },
                "visibility": {
                    "type": "string",
                    "enum": RoomVisibility::SCHEMA_VALUES,
                    "description": tool_msg("tool-channel-room-param-visibility")
                },
                "encryption": {
                    "type": "boolean",
                    "description": tool_msg("tool-channel-room-param-encryption")
                },
                "room_id": {
                    "type": "string",
                    "description": tool_msg("tool-channel-room-param-room-id")
                },
                "user_id": {
                    "type": "string",
                    "description": tool_msg("tool-channel-room-param-user-id")
                }
            },
            "required": ["action", "channel"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "channel_room")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_msg_with_args(
                    "tool-channel-room-error-security",
                    &[("err", &error)],
                )),
            });
        }

        let action = required_string(&args, "action")?;
        let action = ChannelRoomAction::from_str(action)?;
        let channel_name = required_string(&args, "channel")?;
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

        match action {
            ChannelRoomAction::CreateRoom => create_room(&args, channel_name, channel).await,
            ChannelRoomAction::InviteUser => invite_user(&args, channel_name, channel).await,
        }
    }
}

impl ChannelRoomTool {
    fn lookup_channel(&self, channel_name: &str) -> Result<Arc<dyn Channel>, String> {
        let map = self.channels.read();
        if map.is_empty() {
            return Err(tool_msg("tool-channel-room-error-not-initialized"));
        }
        map.get(channel_name).cloned().ok_or_else(|| {
            let mut available: Vec<String> = map.keys().cloned().collect();
            available.sort();
            let available = available.join(", ");
            tool_msg_with_args(
                "tool-channel-room-error-channel-not-found",
                &[("channel", channel_name), ("available", &available)],
            )
        })
    }
}

async fn create_room(
    args: &serde_json::Value,
    channel_name: &str,
    channel: Arc<dyn Channel>,
) -> anyhow::Result<ToolResult> {
    let options = room_creation_options(args)?;
    match channel.create_room(&options).await {
        Ok(room_id) => Ok(ToolResult {
            success: true,
            output: json!({
                "action": ChannelRoomAction::CreateRoom.as_str(),
                "channel": channel_name,
                "room_id": room_id,
            })
            .to_string()
            .into(),
            error: None,
        }),
        Err(error) => {
            let error = error.to_string();
            Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_msg_with_args(
                    "tool-channel-room-error-create-failed",
                    &[("err", &error)],
                )),
            })
        }
    }
}

async fn invite_user(
    args: &serde_json::Value,
    channel_name: &str,
    channel: Arc<dyn Channel>,
) -> anyhow::Result<ToolResult> {
    let room_id = required_string(args, "room_id")?;
    let user_id = required_string(args, "user_id")?;

    match channel.invite_user(room_id, user_id).await {
        Ok(()) => Ok(ToolResult {
            success: true,
            output: json!({
                "action": ChannelRoomAction::InviteUser.as_str(),
                "channel": channel_name,
                "room_id": room_id,
                "user_id": user_id,
            })
            .to_string()
            .into(),
            error: None,
        }),
        Err(error) => {
            let error = error.to_string();
            Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_msg_with_args(
                    "tool-channel-room-error-invite-failed",
                    &[("err", &error)],
                )),
            })
        }
    }
}

fn room_creation_options(args: &serde_json::Value) -> anyhow::Result<RoomCreationOptions> {
    let invites = match args.get("invites") {
        Some(value) => value
            .as_array()
            .ok_or_else(|| anyhow::Error::msg(tool_msg("tool-channel-room-error-invites-array")))?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| {
                        anyhow::Error::msg(tool_msg("tool-channel-room-error-invites-item"))
                    })
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
        None => Vec::new(),
    };

    let visibility = match optional_string(args, "visibility")? {
        Some(value) => Some(RoomVisibility::from_str(value).map_err(|error| {
            let error = error.to_string();
            anyhow::Error::msg(tool_msg_with_args(
                "tool-channel-room-error-invalid-visibility",
                &[("err", &error)],
            ))
        })?),
        None => None,
    };

    Ok(RoomCreationOptions {
        name: optional_string(args, "name")?.map(str::to_string),
        topic: optional_string(args, "topic")?.map(str::to_string),
        invites,
        visibility,
        encryption: optional_bool(args, "encryption")?,
    })
}

fn required_string<'a>(args: &'a serde_json::Value, key: &str) -> anyhow::Result<&'a str> {
    optional_string(args, key)?.ok_or_else(|| {
        anyhow::Error::msg(tool_msg_with_args(
            "tool-channel-room-error-missing-param",
            &[("param", key)],
        ))
    })
}

fn optional_string<'a>(args: &'a serde_json::Value, key: &str) -> anyhow::Result<Option<&'a str>> {
    match args.get(key) {
        None => Ok(None),
        Some(serde_json::Value::String(value)) => {
            let value = value.trim();
            Ok((!value.is_empty()).then_some(value))
        }
        Some(_) => Err(anyhow::Error::msg(tool_msg_with_args(
            "tool-channel-room-error-string-param",
            &[("param", key)],
        ))),
    }
}

fn optional_bool(args: &serde_json::Value, key: &str) -> anyhow::Result<Option<bool>> {
    match args.get(key) {
        None => Ok(None),
        Some(serde_json::Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(anyhow::Error::msg(tool_msg_with_args(
            "tool-channel-room-error-bool-param",
            &[("param", key)],
        ))),
    }
}

fn tool_msg(key: &str) -> String {
    crate::i18n::get_required_tool_string(key)
}

fn tool_msg_with_args(key: &str, args: &[(&str, &str)]) -> String {
    crate::i18n::get_required_tool_string_with_args(key, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use parking_lot::Mutex;
    use parking_lot::RwLock;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zeroclaw_api::channel::{ChannelMessage, SendMessage};

    struct MockChannel {
        created: AtomicUsize,
        invited: AtomicUsize,
        last_options: Mutex<Option<RoomCreationOptions>>,
        last_invite: Mutex<Option<(String, String)>>,
        fail_create: bool,
    }

    impl MockChannel {
        fn new() -> Self {
            Self {
                created: AtomicUsize::new(0),
                invited: AtomicUsize::new(0),
                last_options: Mutex::new(None),
                last_invite: Mutex::new(None),
                fail_create: false,
            }
        }

        fn failing_create() -> Self {
            Self {
                fail_create: true,
                ..Self::new()
            }
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for MockChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::Matrix,
            )
        }
        fn alias(&self) -> &str {
            "test"
        }
    }

    #[async_trait]
    impl Channel for MockChannel {
        fn name(&self) -> &str {
            "matrix"
        }

        async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn create_room(&self, options: &RoomCreationOptions) -> anyhow::Result<String> {
            if self.fail_create {
                anyhow::bail!("homeserver denied room creation");
            }
            self.created.fetch_add(1, Ordering::SeqCst);
            *self.last_options.lock() = Some(options.clone());
            Ok("!new:example.org".to_string())
        }

        async fn invite_user(&self, room_id: &str, user_id: &str) -> anyhow::Result<()> {
            self.invited.fetch_add(1, Ordering::SeqCst);
            *self.last_invite.lock() = Some((room_id.to_string(), user_id.to_string()));
            Ok(())
        }
    }

    fn make_tool_with_channels(channels: Vec<(&str, Arc<dyn Channel>)>) -> ChannelRoomTool {
        let handle = Arc::new(RwLock::new(HashMap::new()));
        {
            let mut map = handle.write();
            for (name, channel) in channels {
                map.insert(name.to_string(), channel);
            }
        }
        ChannelRoomTool::new(Arc::new(SecurityPolicy::default()), handle)
    }

    #[test]
    fn tool_metadata() {
        let tool = ChannelRoomTool::new(
            Arc::new(SecurityPolicy::default()),
            Arc::new(RwLock::new(HashMap::new())),
        );
        assert_eq!(tool.name(), "channel_room");
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["action"].is_object());
        assert!(schema["properties"]["channel"].is_object());
        assert!(schema["properties"]["visibility"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|value| value == "action"));
        assert!(required.iter().any(|value| value == "channel"));
    }

    #[tokio::test]
    async fn create_room_success_passes_typed_options() {
        let mock = Arc::new(MockChannel::new());
        let channel: Arc<dyn Channel> = mock.clone();
        let tool = make_tool_with_channels(vec![("matrix.default", channel)]);

        let result = tool
            .execute(json!({
                "action": "create_room",
                "channel": "matrix.default",
                "name": "Ops",
                "topic": "Incidents",
                "invites": ["@alice:example.org"],
                "visibility": "private",
                "encryption": true
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("!new:example.org"));
        assert_eq!(mock.created.load(Ordering::SeqCst), 1);
        let options = mock.last_options.lock().clone().unwrap();
        assert_eq!(options.name.as_deref(), Some("Ops"));
        assert_eq!(options.topic.as_deref(), Some("Incidents"));
        assert_eq!(options.invites, vec!["@alice:example.org"]);
        assert_eq!(options.visibility, Some(RoomVisibility::Private));
        assert_eq!(options.encryption, Some(true));
    }

    #[tokio::test]
    async fn invite_user_success_passes_room_and_user() {
        let mock = Arc::new(MockChannel::new());
        let channel: Arc<dyn Channel> = mock.clone();
        let tool = make_tool_with_channels(vec![("matrix", channel)]);

        let result = tool
            .execute(json!({
                "action": "invite_user",
                "channel": "matrix",
                "room_id": "!room:example.org",
                "user_id": "@alice:example.org"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(mock.invited.load(Ordering::SeqCst), 1);
        assert_eq!(
            *mock.last_invite.lock(),
            Some(("!room:example.org".into(), "@alice:example.org".into()))
        );
    }

    #[tokio::test]
    async fn unknown_channel_returns_available_channels() {
        let tool = make_tool_with_channels(vec![(
            "matrix",
            Arc::new(MockChannel::new()) as Arc<dyn Channel>,
        )]);

        let result = tool
            .execute(json!({
                "action": "create_room",
                "channel": "slack"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        let error = result.error.as_deref().unwrap();
        assert!(error.contains("not found"));
        assert!(error.contains("matrix"));
    }

    #[tokio::test]
    async fn invalid_visibility_returns_error() {
        let tool = make_tool_with_channels(vec![(
            "matrix",
            Arc::new(MockChannel::new()) as Arc<dyn Channel>,
        )]);

        let err = tool
            .execute(json!({
                "action": "create_room",
                "channel": "matrix",
                "visibility": "shared"
            }))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("expected private or public"));
    }

    #[tokio::test]
    async fn malformed_optional_fields_return_errors() {
        let cases = [
            (
                json!({
                    "action": "create_room",
                    "channel": "matrix",
                    "encryption": "true"
                }),
                "'encryption' must be a boolean",
            ),
            (
                json!({
                    "action": "create_room",
                    "channel": "matrix",
                    "visibility": true
                }),
                "'visibility' must be a string",
            ),
            (
                json!({
                    "action": "create_room",
                    "channel": "matrix",
                    "name": ["Ops"]
                }),
                "'name' must be a string",
            ),
            (
                json!({
                    "action": "create_room",
                    "channel": "matrix",
                    "topic": 42
                }),
                "'topic' must be a string",
            ),
        ];

        for (args, expected_error) in cases {
            let tool = make_tool_with_channels(vec![(
                "matrix",
                Arc::new(MockChannel::new()) as Arc<dyn Channel>,
            )]);

            let err = tool.execute(args).await.unwrap_err();

            assert!(
                err.to_string().contains(expected_error),
                "expected {expected_error:?}, got {err}"
            );
        }
    }

    #[tokio::test]
    async fn channel_error_returns_failed_tool_result() {
        let tool = make_tool_with_channels(vec![(
            "matrix",
            Arc::new(MockChannel::failing_create()) as Arc<dyn Channel>,
        )]);

        let result = tool
            .execute(json!({
                "action": "create_room",
                "channel": "matrix"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("homeserver denied")
        );
    }

    #[tokio::test]
    async fn empty_channels_returns_not_initialized() {
        let tool = ChannelRoomTool::new(
            Arc::new(SecurityPolicy::default()),
            Arc::new(RwLock::new(HashMap::new())),
        );

        let result = tool
            .execute(json!({
                "action": "create_room",
                "channel": "matrix"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("not initialized"));
    }

    #[tokio::test]
    async fn missing_invite_fields_return_error() {
        let tool = make_tool_with_channels(vec![(
            "matrix",
            Arc::new(MockChannel::new()) as Arc<dyn Channel>,
        )]);

        let err = tool
            .execute(json!({
                "action": "invite_user",
                "channel": "matrix",
                "room_id": "!room:example.org"
            }))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("Missing 'user_id' parameter"));
    }
}
